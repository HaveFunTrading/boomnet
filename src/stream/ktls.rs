use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use foreign_types_shared::ForeignType;
use openssl::ssl::ErrorCode;
use std::ffi::c_int;
use std::io;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;

const BIO_NOCLOSE: c_int = 0x00;

pub struct KtlSteam<S> {
    _stream: S,
    ssl: openssl::ssl::Ssl,
}

impl<S> KtlSteam<S> {
    /// Create a new SslStream from a tcp stream and SSL object.
    pub fn new(stream: S, ssl: openssl::ssl::Ssl) -> Self
    where
        S: AsRawFd,
    {
        let sock_bio = unsafe { openssl_sys::BIO_new_socket(stream.as_raw_fd(), BIO_NOCLOSE) };
        assert!(!sock_bio.is_null(), "Failed to create socket BIO");
        unsafe {
            openssl_sys::SSL_set_bio(ssl.as_ptr(), sock_bio, sock_bio);
        }
        KtlSteam { _stream: stream, ssl }
    }

    pub fn connect(&self) -> Result<(), error::Error> {
        let result = unsafe { openssl_sys::SSL_connect(self.ssl.as_ptr()) };
        if result <= 0 {
            Err(error::Error::make(result, &self.ssl))
        } else {
            Ok(())
        }
    }

    pub fn blocking_connect(&self) -> io::Result<()> {
        loop {
            match self.connect() {
                Ok(_) => break,
                Err(err) if err.code() == ErrorCode::WANT_READ => {}
                Err(err) if err.code() == ErrorCode::WANT_WRITE => {}
                Err(err) => return Err(io::Error::other(err)),
            }
        }
        Ok(())
    }

    pub fn ktls_send_enabled(&self) -> bool {
        unsafe {
            let wbio = openssl_sys::SSL_get_wbio(self.ssl.as_ptr());
            ffi::BIO_get_ktls_send(wbio) != 0
        }
    }

    pub fn ktls_recv_enabled(&self) -> bool {
        unsafe {
            let rbio = openssl_sys::SSL_get_rbio(self.ssl.as_ptr());
            ffi::BIO_get_ktls_recv(rbio) != 0
        }
    }

    #[inline]
    pub fn ssl_read(&mut self, buf: &mut [u8]) -> Result<usize, error::Error> {
        unsafe {
            let len =
                openssl_sys::SSL_read(self.ssl.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len().try_into().unwrap());
            if len < 0 {
                Err(error::Error::make(len, &self.ssl))
            } else {
                Ok(len as usize)
            }
        }
    }

    #[inline]
    pub fn ssl_write(&mut self, buf: &[u8]) -> Result<usize, error::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        unsafe {
            let len =
                openssl_sys::SSL_write(self.ssl.as_ptr(), buf.as_ptr() as *const _, buf.len().try_into().unwrap());
            if len < 0 {
                Err(error::Error::make(len, &self.ssl))
            } else {
                Ok(len as usize)
            }
        }
    }
}

impl<S: ConnectionInfoProvider> ConnectionInfoProvider for KtlSteam<S> {
    fn connection_info(&self) -> &ConnectionInfo {
        self._stream.connection_info()
    }
}

impl<S> Read for KtlSteam<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.ssl_read(buf) {
            Ok(read) => Ok(read),
            Err(err) if err.code() == ErrorCode::WANT_READ => Err(ErrorKind::WouldBlock.into()),
            Err(err) if err.code() == ErrorCode::WANT_WRITE => Err(ErrorKind::WouldBlock.into()),
            Err(err) => Err(io::Error::other(err)),
        }
    }
}

impl<S> Write for KtlSteam<S> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.ssl_write(buf) {
            Ok(read) => Ok(read),
            Err(err) if err.code() == ErrorCode::WANT_READ => Err(ErrorKind::WouldBlock.into()),
            Err(err) if err.code() == ErrorCode::WANT_WRITE => Err(ErrorKind::WouldBlock.into()),
            Err(err) => Err(io::Error::other(err)),
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub mod error {
    use std::{error, ffi::c_int, fmt, io};

    use openssl::{error::ErrorStack, ssl::ErrorCode};

    #[derive(Debug)]
    pub(crate) enum InnerError {
        Io(io::Error),
        Ssl(ErrorStack),
    }

    /// An SSL error.
    #[derive(Debug)]
    pub struct Error {
        pub(crate) code: ErrorCode,
        pub(crate) cause: Option<InnerError>,
    }

    impl Error {
        pub fn code(&self) -> ErrorCode {
            self.code
        }

        pub fn io_error(&self) -> Option<&io::Error> {
            match self.cause {
                Some(InnerError::Io(ref e)) => Some(e),
                _ => None,
            }
        }

        pub fn into_io_error(self) -> Result<io::Error, Error> {
            match self.cause {
                Some(InnerError::Io(e)) => Ok(e),
                _ => Err(self),
            }
        }

        pub fn ssl_error(&self) -> Option<&ErrorStack> {
            match self.cause {
                Some(InnerError::Ssl(ref e)) => Some(e),
                _ => None,
            }
        }

        pub(crate) fn make(ret: c_int, ssl: &openssl::ssl::SslRef) -> Self {
            use foreign_types_shared::ForeignTypeRef;
            let code = unsafe { ErrorCode::from_raw(openssl_sys::SSL_get_error(ssl.as_ptr(), ret)) };

            let cause = match code {
                ErrorCode::SSL => Some(InnerError::Ssl(ErrorStack::get())),
                ErrorCode::SYSCALL => {
                    let errs = ErrorStack::get();
                    if errs.errors().is_empty() {
                        // get last error from io
                        let e = std::io::Error::last_os_error();
                        Some(InnerError::Io(e))
                    } else {
                        Some(InnerError::Ssl(errs))
                    }
                }
                ErrorCode::ZERO_RETURN => None,
                ErrorCode::WANT_READ | ErrorCode::WANT_WRITE => {
                    // get last error from io
                    let e = std::io::Error::last_os_error();
                    Some(InnerError::Io(e))
                }
                _ => None,
            };

            Error { code, cause }
        }

        pub(crate) fn make_from_io(e: io::Error) -> Self {
            Error {
                code: ErrorCode::SYSCALL,
                cause: Some(InnerError::Io(e)),
            }
        }
    }

    impl From<ErrorStack> for Error {
        fn from(e: ErrorStack) -> Error {
            Error {
                code: ErrorCode::SSL,
                cause: Some(InnerError::Ssl(e)),
            }
        }
    }

    impl fmt::Display for Error {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.code {
                ErrorCode::ZERO_RETURN => fmt.write_str("the SSL session has been shut down"),
                ErrorCode::WANT_READ => match self.io_error() {
                    Some(_) => fmt.write_str("a nonblocking read call would have blocked"),
                    None => fmt.write_str("the operation should be retried"),
                },
                ErrorCode::WANT_WRITE => match self.io_error() {
                    Some(_) => fmt.write_str("a nonblocking write call would have blocked"),
                    None => fmt.write_str("the operation should be retried"),
                },
                ErrorCode::SYSCALL => match self.io_error() {
                    Some(err) => write!(fmt, "{err}"),
                    None => fmt.write_str("unexpected EOF"),
                },
                ErrorCode::SSL => match self.ssl_error() {
                    Some(e) => write!(fmt, "{e}"),
                    None => fmt.write_str("OpenSSL error"),
                },
                _ => write!(fmt, "unknown error code {}", self.code.as_raw()),
            }
        }
    }

    impl error::Error for Error {
        fn source(&self) -> Option<&(dyn error::Error + 'static)> {
            match self.cause {
                Some(InnerError::Io(ref e)) => Some(e),
                Some(InnerError::Ssl(ref e)) => Some(e),
                None => None,
            }
        }
    }
}

mod ffi {
    use openssl_sys::BIO_ctrl;
    use std::ffi::{c_int, c_long};

    const BIO_CTRL_GET_KTLS_SEND: c_int = 73;
    const BIO_CTRL_GET_KTLS_RECV: c_int = 76;

    #[allow(non_snake_case)]
    pub unsafe fn BIO_get_ktls_send(b: *mut openssl_sys::BIO) -> c_long {
        unsafe { BIO_ctrl(b, BIO_CTRL_GET_KTLS_SEND, 0, std::ptr::null_mut()) }
    }
    #[allow(non_snake_case)]
    pub unsafe fn BIO_get_ktls_recv(b: *mut openssl_sys::BIO) -> c_long {
        unsafe { BIO_ctrl(b, BIO_CTRL_GET_KTLS_RECV, 0, std::ptr::null_mut()) }
    }
}
