//! Provides TLS offload to the kernel (KTLS).
//!
use crate::service::select::Selectable;
use crate::stream::ktls::error::Error;
use crate::stream::ktls::net::peer_addr;
use crate::stream::tls::TlsConfig;
use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use foreign_types::ForeignType;
#[cfg(feature = "mio")]
use mio::{Interest, Registry, Token, event::Source};
use openssl::ssl::{ErrorCode, SslOptions};
use smallstr::SmallString;
use std::io;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr::slice_from_raw_parts;

/// Offloads TLS to the kernel (KTLS). Uses OpenSSL backend to configure KTLS post handshake (can change in the future).
/// The stream is designed to work with a non-blocking underlying stream.
///
/// ## Prerequisites
/// Ensure that `tls` kernel module is installed. Otherwise, the code will panic if either KTLS
/// `send` or `recv` are not enabled. This is the minimum required to enable KTLS in the
/// software mode.
///
/// ## Example
/// ```no_run
/// use boomnet::stream::tcp::TcpStream;
/// use crate::boomnet::stream::ktls::IntoKtlsStream;
///
/// let ktls_stream = TcpStream::try_from(("fstream.binance.com", 443)).unwrap().into_ktls_stream().unwrap();
/// ```
pub struct KtlsStream<S> {
    stream: S,
    ssl: openssl::ssl::Ssl,
    state: State,
    buffer: Vec<u8>,
}

impl<S> KtlsStream<S> {
    /// Create KTLS from underlying stream using default [`TlsConfig`].
    pub fn new(stream: S, server_name: impl AsRef<str>) -> io::Result<KtlsStream<S>>
    where
        S: AsRawFd,
    {
        Self::new_with_config(stream, server_name, |_| ())
    }

    /// Create KTLS from underlying stream. This method also requires an action used
    /// further configure [`TlsConfig`].
    pub fn new_with_config<F>(stream: S, server_name: impl AsRef<str>, configure: F) -> io::Result<KtlsStream<S>>
    where
        S: AsRawFd,
        F: FnOnce(&mut TlsConfig),
    {
        const SSL_OP_ENABLE_KTLS: SslOptions = SslOptions::from_bits_retain(ffi::SSL_OP_ENABLE_KTLS);

        let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls_client())?;
        builder.set_options(SSL_OP_ENABLE_KTLS);

        let mut tls_config = builder.into();
        configure(&mut tls_config);

        let config = tls_config.into_openssl().build().configure()?;
        let ssl = config.into_ssl(server_name.as_ref())?;

        Ok(KtlsStream {
            stream,
            ssl,
            state: State::Connecting,
            buffer: Vec::with_capacity(4096),
        })
    }

    #[inline]
    fn connected(&self) -> io::Result<bool>
    where
        S: AsRawFd,
    {
        let fd = unsafe { BorrowedFd::borrow_raw(self.stream.as_raw_fd()) };
        Ok(peer_addr(fd)?.is_some())
    }

    #[inline]
    fn ssl_connect(&self) -> Result<(), Error> {
        let result = unsafe { openssl_sys::SSL_connect(self.ssl.as_ptr()) };
        if result <= 0 {
            Err(Error::make(result, &self.ssl))
        } else {
            Ok(())
        }
    }

    fn ktls_send_enabled(&self) -> bool {
        unsafe {
            let wbio = openssl_sys::SSL_get_wbio(self.ssl.as_ptr());
            ffi::BIO_get_ktls_send(wbio) != 0
        }
    }

    fn ktls_recv_enabled(&self) -> bool {
        unsafe {
            let rbio = openssl_sys::SSL_get_rbio(self.ssl.as_ptr());
            ffi::BIO_get_ktls_recv(rbio) != 0
        }
    }

    #[inline]
    fn ssl_read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
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
    fn ssl_write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        unsafe {
            let len =
                openssl_sys::SSL_write(self.ssl.as_ptr(), buf.as_ptr() as *const _, buf.len().try_into().unwrap());
            if len < 0 {
                Err(Error::make(len, &self.ssl))
            } else {
                Ok(len as usize)
            }
        }
    }
}

#[derive(Copy, Clone)]
enum State {
    Connecting,
    Handshake,
    Drain(usize),
    Ready,
}

impl<S: ConnectionInfoProvider> ConnectionInfoProvider for KtlsStream<S> {
    fn connection_info(&self) -> &ConnectionInfo {
        self.stream.connection_info()
    }
}

impl<S: AsRawFd> Read for KtlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.state {
            State::Connecting => {
                if self.connected()? {
                    // we intentionally pass BIO_NO_CLOSE to prevent double free on the file descriptor
                    let sock_bio = unsafe { openssl_sys::BIO_new_socket(self.stream.as_raw_fd(), ffi::BIO_NO_CLOSE) };
                    assert!(!sock_bio.is_null(), "failed to create socket BIO");
                    unsafe {
                        openssl_sys::SSL_set_bio(self.ssl.as_ptr(), sock_bio, sock_bio);
                    }
                    self.state = State::Handshake;
                }
            }
            State::Handshake => match self.ssl_connect() {
                Ok(_) => {
                    assert!(self.ktls_recv_enabled(), "ktls recv not enabled, did you install 'tls' kernel module?");
                    assert!(self.ktls_send_enabled(), "ktls send not enabled, did you install 'tls' kernel module?");
                    self.state = State::Drain(0)
                }
                Err(err) if err.code() == ErrorCode::WANT_READ => {}
                Err(err) if err.code() == ErrorCode::WANT_WRITE => {}
                Err(err) => return Err(io::Error::other(err)),
            },
            State::Drain(index) => {
                let mut from = index;
                let remaining =
                    unsafe { &*slice_from_raw_parts(self.buffer.as_ptr().add(from), self.buffer.len() - from) };
                if remaining.is_empty() {
                    self.state = State::Ready;
                } else {
                    from += match self.ssl_write(remaining) {
                        Ok(len) => len,
                        Err(err) if err.code() == ErrorCode::WANT_READ => 0,
                        Err(err) if err.code() == ErrorCode::WANT_WRITE => 0,
                        Err(err) => return Err(io::Error::other(err)),
                    };
                    self.state = State::Drain(from);
                }
            }
            State::Ready => match self.ssl_read(buf) {
                Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
                Ok(len) => return Ok(len),
                Err(err) if err.code() == ErrorCode::WANT_READ => {}
                Err(err) if err.code() == ErrorCode::WANT_WRITE => {}
                Err(err) => return Err(io::Error::other(err)),
            },
        }
        Err(ErrorKind::WouldBlock.into())
    }
}

impl<S: Write> Write for KtlsStream<S> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.state {
            State::Ready => match self.ssl_write(buf) {
                Ok(len) => Ok(len),
                Err(err) if err.code() == ErrorCode::WANT_READ => Err(ErrorKind::WouldBlock.into()),
                Err(err) if err.code() == ErrorCode::WANT_WRITE => Err(ErrorKind::WouldBlock.into()),
                Err(err) => Err(io::Error::other(err)),
            },
            _ => {
                // we buffer any pending write
                self.buffer.extend_from_slice(buf);
                Ok(buf.len())
            }
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        match self.state {
            State::Connecting | State::Handshake | State::Drain(_) => Ok(()),
            State::Ready => self.stream.flush(),
        }
    }
}

impl<S: Selectable> Selectable for KtlsStream<S> {
    #[inline]
    fn connected(&mut self) -> io::Result<bool> {
        self.stream.connected()
    }

    #[inline]
    fn make_writable(&mut self) -> io::Result<()> {
        self.stream.make_writable()
    }

    #[inline]
    fn make_readable(&mut self) -> io::Result<()> {
        self.stream.make_readable()
    }
}

#[cfg(feature = "mio")]
impl<S: Source> Source for KtlsStream<S> {
    #[inline]
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        registry.register(&mut self.stream, token, interests)
    }

    #[inline]
    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        registry.reregister(&mut self.stream, token, interests)
    }

    #[inline]
    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        registry.deregister(&mut self.stream)
    }
}

/// Trait to convert underlying stream into [`KtlsStream`].
pub trait IntoKtlsStream {
    /// Convert underlying stream into [`KtlsStream`] with default tls config.
    ///
    /// ## Examples
    /// ```no_run
    /// use boomnet::stream::tcp::TcpStream;
    /// use boomnet::stream::ktls::IntoKtlsStream;
    ///
    /// let ktls = TcpStream::try_from(("127.0.0.1", 4222)).unwrap().into_ktls_stream().unwrap();
    /// ```
    fn into_ktls_stream(self) -> io::Result<KtlsStream<Self>>
    where
        Self: Sized,
    {
        self.into_ktls_stream_with_config(|_| ())
    }

    /// Convert underlying stream into [`KtlsStream`] and use provided action to modify tls config.
    ///
    /// ## Examples
    /// ```no_run
    /// use boomnet::stream::tcp::TcpStream;
    /// use boomnet::stream::ktls::IntoKtlsStream;
    /// use boomnet::stream::tls::TlsConfigExt;
    ///
    /// let ktls = TcpStream::try_from(("127.0.0.1", 4222)).unwrap().into_ktls_stream_with_config(|cfg| cfg.with_no_cert_verification()).unwrap();
    /// ```
    fn into_ktls_stream_with_config<F>(self, builder: F) -> io::Result<KtlsStream<Self>>
    where
        Self: Sized,
        F: FnOnce(&mut TlsConfig);
}

impl<T> IntoKtlsStream for T
where
    T: Read + Write + AsRawFd + ConnectionInfoProvider,
{
    fn into_ktls_stream_with_config<F>(self, builder: F) -> io::Result<KtlsStream<Self>>
    where
        Self: Sized,
        F: FnOnce(&mut TlsConfig),
    {
        let server_name = SmallString::<[u8; 1024]>::from(self.connection_info().host());
        KtlsStream::new_with_config(self, server_name, builder)
    }
}

mod error {
    use foreign_types::ForeignTypeRef;
    use openssl::{error::ErrorStack, ssl::ErrorCode};
    use std::{error, ffi::c_int, fmt, io};

    #[derive(Debug)]
    enum InnerError {
        Io(io::Error),
        Ssl(ErrorStack),
    }

    /// An SSL error.
    #[derive(Debug)]
    pub struct Error {
        code: ErrorCode,
        cause: Option<InnerError>,
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

        pub fn ssl_error(&self) -> Option<&ErrorStack> {
            match self.cause {
                Some(InnerError::Ssl(ref e)) => Some(e),
                _ => None,
            }
        }

        pub fn make(ret: c_int, ssl: &openssl::ssl::SslRef) -> Self {
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

    pub const SSL_OP_ENABLE_KTLS: u64 = 0x00000008;
    pub const BIO_NO_CLOSE: c_int = 0x00;
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

mod net {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::os::fd::{AsRawFd, BorrowedFd};
    use std::{io, mem};

    pub fn peer_addr(fd: BorrowedFd<'_>) -> io::Result<Option<SocketAddr>> {
        let raw = fd.as_raw_fd();

        let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
        let mut len = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;

        let rc = unsafe { libc::getpeername(raw, &mut storage as *mut _ as *mut libc::sockaddr, &mut len as *mut _) };

        if rc == -1 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOTCONN) {
                return Ok(None);
            }
            return Err(err);
        }

        unsafe {
            match storage.ss_family as libc::c_int {
                libc::AF_INET => {
                    let sa = &*(&storage as *const _ as *const libc::sockaddr_in);
                    let ip = Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr));
                    let port = u16::from_be(sa.sin_port);
                    Ok(Some(SocketAddr::new(IpAddr::V4(ip), port)))
                }
                libc::AF_INET6 => {
                    let sa = &*(&storage as *const _ as *const libc::sockaddr_in6);
                    let ip = Ipv6Addr::from(sa.sin6_addr.s6_addr);
                    let port = u16::from_be(sa.sin6_port);
                    Ok(Some(SocketAddr::new(IpAddr::V6(ip), port)))
                }
                _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported address family")),
            }
        }
    }
}
