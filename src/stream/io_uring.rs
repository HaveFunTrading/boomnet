//! Readiness-gated TCP stream for [`IoUringSelector`](crate::service::select::io_uring::IoUringSelector).

use crate::service::select::Selectable;
use crate::stream::{ConnectionInfo, ConnectionInfoProvider, ReadHint};
use std::io::ErrorKind::{Interrupted, NotConnected, WouldBlock, WriteZero};
use std::io::{self, Read, Write};
use std::net;
use std::os::fd::{AsRawFd, RawFd};

/// A nonblocking TCP stream whose reads are enabled by `io_uring` readiness completions.
///
/// Before the selector reports the socket readable, [`Read::read`] returns `WouldBlock`
/// without entering the kernel. Readability is cleared only after the socket itself returns
/// `EAGAIN`, so short TCP reads do not accidentally suppress unread data.
#[derive(Debug)]
pub struct IoUringStream {
    inner: net::TcpStream,
    connection_info: ConnectionInfo,
    connected: bool,
    can_read: bool,
    can_write: bool,
    pending_write: Vec<u8>,
    pending_write_offset: usize,
}

impl IoUringStream {
    fn new(inner: net::TcpStream, connection_info: ConnectionInfo) -> Self {
        Self {
            inner,
            connection_info,
            connected: false,
            can_read: false,
            can_write: false,
            pending_write: Vec::with_capacity(4096),
            pending_write_offset: 0,
        }
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        while self.pending_write_offset < self.pending_write.len() {
            match self.inner.write(&self.pending_write[self.pending_write_offset..]) {
                Ok(0) => return Err(io::Error::from(WriteZero)),
                Ok(written) => self.pending_write_offset += written,
                Err(error) if error.kind() == Interrupted => continue,
                Err(error) if error.kind() == WouldBlock => {
                    self.can_write = false;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        self.pending_write.clear();
        self.pending_write_offset = 0;
        Ok(())
    }
}

impl AsRawFd for IoUringStream {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

impl Selectable for IoUringStream {
    fn connected(&mut self) -> io::Result<bool> {
        if self.connected {
            return Ok(true);
        }
        if let Some(error) = self.inner.take_error()? {
            return Err(error);
        }
        match self.inner.peer_addr() {
            Ok(_) => {
                self.connected = true;
                Ok(true)
            }
            Err(error) if matches!(error.kind(), NotConnected | Interrupted) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn make_writable(&mut self) -> io::Result<()> {
        self.can_write = true;
        self.flush_pending()
    }

    fn make_readable(&mut self) -> io::Result<()> {
        self.can_read = true;
        Ok(())
    }
}

impl ReadHint for IoUringStream {
    #[inline]
    fn read_hint(&self) -> bool {
        self.can_read
    }
}

impl Read for IoUringStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.can_read {
            return Err(io::Error::from(WouldBlock));
        }
        loop {
            match self.inner.read(buf) {
                Ok(0) => {
                    self.can_read = false;
                    return Ok(0);
                }
                result @ Ok(_) => return result,
                Err(error) if error.kind() == Interrupted => continue,
                Err(error) if error.kind() == WouldBlock => {
                    self.can_read = false;
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Write for IoUringStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.can_write {
            self.pending_write.extend_from_slice(buf);
            return Ok(buf.len());
        }
        match self.inner.write(buf) {
            Err(error) if error.kind() == WouldBlock => {
                self.can_write = false;
                Err(error)
            }
            result => result,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ConnectionInfoProvider for IoUringStream {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

/// Convert a Boomnet TCP stream into an [`IoUringStream`].
pub trait IntoIoUringStream {
    fn into_io_uring_stream(self) -> IoUringStream;
}

impl<T> IntoIoUringStream for T
where
    T: Into<net::TcpStream>,
    T: ConnectionInfoProvider,
{
    fn into_io_uring_stream(self) -> IoUringStream {
        let connection_info = self.connection_info().clone();
        IoUringStream::new(self.into(), connection_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::tcp;
    use std::net::TcpListener;

    fn connected_pair() -> (IoUringStream, net::TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let client = tcp::TcpStream::new(client, ConnectionInfo::new("localhost", 1));
        (client.into_io_uring_stream(), server)
    }

    #[test]
    fn read_is_gated_until_readable_and_cleared_only_by_eagain() {
        let (mut client, mut server) = connected_pair();
        let mut buffer = [0; 16];
        assert_eq!(client.read(&mut buffer).unwrap_err().kind(), WouldBlock);

        server.write_all(b"abc").unwrap();
        client.make_readable().unwrap();
        assert_eq!(client.read(&mut buffer).unwrap(), 3);
        assert!(client.can_read, "a short read must not clear readiness");
        assert_eq!(client.read(&mut buffer).unwrap_err().kind(), WouldBlock);
        assert!(!client.can_read);
    }

    #[test]
    fn writes_before_connect_readiness_are_flushed_when_writable() {
        let (mut client, mut server) = connected_pair();
        client.write_all(b"hello").unwrap();
        assert!(client.connected().unwrap());
        client.make_writable().unwrap();

        let mut buffer = [0; 5];
        server.set_nonblocking(false).unwrap();
        server.read_exact(&mut buffer).unwrap();
        assert_eq!(&buffer, b"hello");
    }
}
