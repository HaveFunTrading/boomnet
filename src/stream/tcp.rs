//! Wrapper over `std::net::TcpStream`.

use crate::service::select::Selectable;
use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use std::io;
use std::io::{Read, Write};
use std::net::SocketAddr;

/// Wraps `std::net::TcpStream` and provides `ConnectionInfo`.
pub struct TcpStream {
    inner: std::net::TcpStream,
    connection_info: ConnectionInfo,
}

impl From<TcpStream> for std::net::TcpStream {
    fn from(stream: TcpStream) -> Self {
        stream.inner
    }
}

impl TryFrom<(&str, u16)> for TcpStream {
    type Error = io::Error;

    fn try_from(host_and_port: (&str, u16)) -> Result<Self, Self::Error> {
        ConnectionInfo::from(host_and_port).try_into()
    }
}

impl TryFrom<ConnectionInfo> for TcpStream {
    type Error = io::Error;

    fn try_from(connection_info: ConnectionInfo) -> Result<Self, Self::Error> {
        connection_info.into_tcp_stream()
    }
}

impl TryFrom<&ConnectionInfo> for TcpStream {
    type Error = io::Error;

    fn try_from(connection_info: &ConnectionInfo) -> Result<Self, Self::Error> {
        connection_info.clone().into_tcp_stream()
    }
}

impl TryFrom<(&ConnectionInfo, SocketAddr)> for TcpStream {
    type Error = io::Error;

    fn try_from(conn_and_addr: (&ConnectionInfo, SocketAddr)) -> Result<Self, Self::Error> {
        let (conn, addr) = conn_and_addr;
        conn.clone().into_tcp_stream_with_addr(addr)
    }
}

impl TryFrom<(ConnectionInfo, SocketAddr)> for TcpStream {
    type Error = io::Error;

    fn try_from(conn_and_addr: (ConnectionInfo, SocketAddr)) -> Result<Self, Self::Error> {
        let (conn, addr) = conn_and_addr;
        conn.into_tcp_stream_with_addr(addr)
    }
}

impl TcpStream {
    pub fn new(stream: std::net::TcpStream, connection_info: ConnectionInfo) -> Self {
        Self {
            inner: stream,
            connection_info,
        }
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Selectable for TcpStream {
    fn connected(&mut self) -> io::Result<bool> {
        Ok(true)
    }

    fn make_writable(&mut self) {
        // no-op
    }

    fn make_readable(&mut self) {
        // no-op
    }
}

impl ConnectionInfoProvider for TcpStream {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}
