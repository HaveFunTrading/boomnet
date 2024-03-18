use std::io;
use std::io::ErrorKind::Other;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

#[cfg(feature = "mio")]
use mio::{event::Source, Interest, Registry, Token};
use rustls::{ClientConnection, RootCertStore};

use crate::select::Selectable;
#[cfg(feature = "mio")]
use crate::stream::mio::MioStream;
use crate::stream::recorder::{IntoRecordedStream, RecordedStream, Recorder};
use crate::util::NoBlock;

pub struct TlsStream<S> {
    stream: S,
    tls: ClientConnection,
}

#[cfg(feature = "mio")]
impl<S: Source> Source for TlsStream<S> {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        registry.register(&mut self.stream, token, interests)
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        registry.reregister(&mut self.stream, token, interests)
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        registry.deregister(&mut self.stream)
    }
}

impl<S: Selectable> Selectable for TlsStream<S> {
    fn connected(&mut self) -> io::Result<bool> {
        self.stream.connected()
    }

    fn make_writable(&mut self) {
        self.stream.make_writable()
    }

    fn make_readable(&mut self) {
        self.stream.make_readable()
    }
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (_, _) = self.complete_io()?;
        self.tls.reader().read(buf)
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tls.writer().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tls.writer().flush()
    }
}

impl<S: Read + Write> TlsStream<S> {
    pub fn wrap(stream: S, server_name: &str) -> TlsStream<S> {
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let tls = ClientConnection::new(Arc::new(config), server_name.to_owned().try_into().unwrap()).unwrap();

        Self { stream, tls }
    }

    fn complete_io(&mut self) -> io::Result<(usize, usize)> {
        let wrote = if self.tls.wants_write() {
            self.tls.write_tls(&mut self.stream)?
        } else {
            0
        };

        let read = if self.tls.wants_read() {
            let read = self.tls.read_tls(&mut self.stream).no_block()?;
            if read > 0 {
                self.tls
                    .process_new_packets()
                    .map_err(|err| io::Error::new(Other, err))?;
            }
            read
        } else {
            0
        };

        Ok((read, wrote))
    }
}

#[allow(clippy::large_enum_variant)]
pub enum TlsReadyStream<S> {
    Plain(S),
    Tls(TlsStream<S>),
}

impl<S: Read + Write> Read for TlsReadyStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            TlsReadyStream::Plain(stream) => stream.read(buf),
            TlsReadyStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl<S: Read + Write> Write for TlsReadyStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            TlsReadyStream::Plain(stream) => stream.write(buf),
            TlsReadyStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => stream.flush(),
            TlsReadyStream::Tls(stream) => stream.flush(),
        }
    }
}

#[cfg(feature = "mio")]
impl<S: Source> Source for TlsReadyStream<S> {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => registry.register(stream, token, interests),
            TlsReadyStream::Tls(stream) => registry.register(stream, token, interests),
        }
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => registry.reregister(stream, token, interests),
            TlsReadyStream::Tls(stream) => registry.reregister(stream, token, interests),
        }
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => registry.deregister(stream),
            TlsReadyStream::Tls(stream) => registry.deregister(stream),
        }
    }
}

impl<S: Selectable> Selectable for TlsReadyStream<S> {
    fn connected(&mut self) -> io::Result<bool> {
        match self {
            TlsReadyStream::Plain(stream) => stream.connected(),
            TlsReadyStream::Tls(stream) => stream.connected(),
        }
    }

    fn make_writable(&mut self) {
        match self {
            TlsReadyStream::Plain(stream) => stream.make_writable(),
            TlsReadyStream::Tls(stream) => stream.make_writable(),
        }
    }

    fn make_readable(&mut self) {
        match self {
            TlsReadyStream::Plain(stream) => stream.make_readable(),
            TlsReadyStream::Tls(stream) => stream.make_readable(),
        }
    }
}

pub trait NotTlsStream {}

impl NotTlsStream for TcpStream {}
impl<S> NotTlsStream for RecordedStream<S> {}

#[cfg(feature = "mio")]
impl NotTlsStream for MioStream {}

pub trait IntoTlsStream {
    fn into_tls_stream(self, server_name: &str) -> TlsStream<Self>
    where
        Self: Sized;
}

impl<T> IntoTlsStream for T
where
    T: Read + Write + NotTlsStream,
{
    fn into_tls_stream(self, server_name: &str) -> TlsStream<Self>
    where
        Self: Sized,
    {
        TlsStream::wrap(self, server_name)
    }
}

impl<T> IntoRecordedStream for TlsStream<T> {
    fn into_recorded_stream(self, recording_name: impl AsRef<str>) -> RecordedStream<Self>
    where
        Self: Sized,
    {
        RecordedStream::new(self, Recorder::new(recording_name).unwrap())
    }
}

impl<T> IntoRecordedStream for TlsReadyStream<T> {
    fn into_recorded_stream(self, recording_name: impl AsRef<str>) -> RecordedStream<Self>
    where
        Self: Sized,
    {
        RecordedStream::new(self, Recorder::new(recording_name).unwrap())
    }
}
