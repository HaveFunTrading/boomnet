use std::io::{Read, Write};
use crate::stream::{ConnectionInfo, ConnectionInfoProvider};

pub struct TcpStream {
    inner: std::net::TcpStream,
    connection_info: ConnectionInfo
}

impl From<TcpStream> for std::net::TcpStream {
    fn from(stream: TcpStream) -> Self {
        stream.inner
    }
}

impl TcpStream {
    pub fn new(stream: std::net::TcpStream, connection_info: ConnectionInfo) -> Self {
        Self {
            inner: stream,
            connection_info
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

impl ConnectionInfoProvider for TcpStream {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}