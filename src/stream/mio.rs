use std::io::ErrorKind::{Interrupted, NotConnected, WouldBlock};
use std::io::{Read, Write};
use std::{io, net};

use crate::service::select::Selectable;
use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use mio::event::Source;
use mio::net::TcpStream;
use mio::{Interest, Registry, Token};

pub struct MioStream {
    inner: TcpStream,
    connection_info: ConnectionInfo,
    connected: bool,
    can_read: bool,
    can_write: bool,
}

impl MioStream {
    fn new(inner: TcpStream, connection_info: ConnectionInfo) -> MioStream {
        Self {
            inner,
            connection_info,
            connected: false,
            can_read: false,
            can_write: false,
        }
    }
}

impl Selectable for MioStream {
    fn connected(&mut self) -> io::Result<bool> {
        if self.connected {
            return Ok(true);
        }

        match self.inner.peer_addr() {
            Ok(_) => {
                self.connected = true;
                Ok(true)
            }
            Err(err) if err.kind() == NotConnected => Ok(false),
            Err(err) if err.kind() == Interrupted => Ok(false),
            Err(err) => Err(err),
        }
    }

    fn make_writable(&mut self) {
        self.can_write = true;
    }

    fn make_readable(&mut self) {
        self.can_read = true;
    }
}

impl Source for MioStream {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        registry.register(&mut self.inner, token, interests)
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        registry.reregister(&mut self.inner, token, interests)
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        registry.deregister(&mut self.inner)
    }
}

impl Read for MioStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.can_read {
            let read = self.inner.read(buf)?;
            if read < buf.len() {
                self.can_read = false;
            }
            return Ok(read);
        }
        Err(io::Error::from(WouldBlock))
    }
}

impl Write for MioStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.can_write {
            return Ok(0);
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ConnectionInfoProvider for MioStream {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

pub trait IntoMioStream {
    fn into_mio_stream(self) -> MioStream;
}

impl<T> IntoMioStream for T
where
    T: Into<net::TcpStream>,
    T: ConnectionInfoProvider,
{
    fn into_mio_stream(self) -> MioStream {
        let connection_info = self.connection_info().clone();
        MioStream::new(TcpStream::from_std(self.into()), connection_info)
    }
}
