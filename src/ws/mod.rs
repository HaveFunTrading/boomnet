use std::io::ErrorKind::{Other, WouldBlock};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::{io, mem};

#[cfg(feature = "mio")]
use mio::{event::Source, Interest, Registry, Token};
use thiserror::Error;
use url::Url;

use crate::buffer;
use crate::select::Selectable;
use crate::stream::tls::{IntoTlsStream, TlsReadyStream, TlsStream};
use crate::ws::decoder::Decoder;
use crate::ws::handshake::{HandshakeState, Handshaker};
use crate::ws::Error::{Closed, ReceivedCloseFrame};

mod decoder;
pub mod ds;
mod encoder;
mod handshake;
mod protocol;

type ReadBuffer = buffer::ReadBuffer<4096>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("the peer has sent the close frame: {0}")]
    ReceivedCloseFrame(String),
    #[error("the websocket is closed and can be dropped")]
    Closed,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

impl From<Error> for io::Error {
    fn from(value: Error) -> Self {
        io::Error::new(Other, value)
    }
}

pub enum WebsocketFrame<'a> {
    Ping(u64, &'a [u8]),
    Pong(u64, &'a [u8]),
    Text(u64, bool, &'a [u8]),
    Binary(u64, bool, &'a [u8]),
    Continuation(u64, bool, &'a [u8]),
    Close(u64, &'a [u8]),
}

#[derive(Debug)]
pub struct Websocket<S> {
    stream: S,
    handshaker: Handshaker,
    frame: Decoder,
    closed: bool,
    pending_pong: bool,
    pong_payload: Vec<u8>,
}

impl<S> Websocket<S> {
    pub fn closed(&self) -> bool {
        self.closed
    }
}

#[cfg(feature = "mio")]
impl<S: Source> Source for Websocket<S> {
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

impl<S: Selectable> Selectable for Websocket<S> {
    fn connected(&mut self) -> io::Result<bool> {
        self.stream.connected()
    }

    fn make_writable(&mut self) {
        self.stream.make_writable();
    }

    fn make_readable(&mut self) {
        self.stream.make_readable();
    }
}

impl<S: Read + Write> Websocket<S> {
    pub fn receive_next(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        self.ensure_not_closed()?;

        // check if we have any pending pong to send
        if self.needs_to_send_pong() {
            self.process_pending_pong()?;
        }

        match self.handshaker.state() {
            HandshakeState::NotStarted => panic!("websocket handshake not started"),
            HandshakeState::Pending => self.perform_handshake(),
            HandshakeState::Completed => self.decode_next_frame(),
        }
    }

    #[cold]
    #[inline(never)]
    fn perform_handshake(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        match self.handshaker.await_handshake(&mut self.stream) {
            Ok(()) => Ok(None),
            Err(err) if err.kind() == WouldBlock => Ok(None),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    fn decode_next_frame(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        match self.frame.decode_next(&mut self.stream) {
            Ok(Some(WebsocketFrame::Ping(_, payload))) => {
                self.pong_payload.extend_from_slice(payload);
                self.pending_pong = true;
                Ok(None)
            }
            Ok(Some(WebsocketFrame::Close(_, payload))) => {
                self.closed = true;
                Err(ReceivedCloseFrame(String::from_utf8_lossy(payload).to_string()))
            }
            Ok(frame) => Ok(frame),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    pub fn send_text(&mut self, fin: bool, body: Option<&[u8]>) -> Result<(), Error> {
        self.send(fin, protocol::op::TEXT_FRAME, body)
    }

    #[inline]
    fn needs_to_send_pong(&self) -> bool {
        self.pending_pong
    }

    #[cold]
    #[inline(never)]
    fn process_pending_pong(&mut self) -> Result<(), Error> {
        let mut pong_payload = mem::take(&mut self.pong_payload);
        let res = self.send_pong(Some(pong_payload.as_slice()));
        pong_payload.clear();
        let _ = mem::replace(&mut self.pong_payload, pong_payload);
        self.pending_pong = false;
        res
    }

    fn send_pong(&mut self, payload: Option<&[u8]>) -> Result<(), Error> {
        self.send(true, protocol::op::PONG, payload)
    }

    fn send(&mut self, fin: bool, op_code: u8, body: Option<&[u8]>) -> Result<(), Error> {
        self.ensure_not_closed()?;

        match encoder::send(&mut self.stream, fin, op_code, body) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    fn ensure_not_closed(&self) -> Result<(), Error> {
        #[cold]
        #[inline(never)]
        fn signal_closed() -> Result<(), Error> {
            Err(Closed)
        }

        if self.closed {
            return signal_closed();
        }

        Ok(())
    }
}

pub trait IntoWebsocket {
    fn into_websocket(self, url: &str) -> Websocket<Self>
    where
        Self: Sized;
}

impl<T> IntoWebsocket for T
where
    T: Read + Write,
{
    fn into_websocket(self, url: &str) -> Websocket<Self>
    where
        Self: Sized,
    {
        Websocket::new(self, url).unwrap()
    }
}

pub trait IntoTlsWebsocket {
    fn into_tls_websocket(self, url: &str) -> Websocket<TlsStream<Self>>
    where
        Self: Sized;
}

impl<T> IntoTlsWebsocket for T
where
    T: Read + Write,
    T: IntoTlsStream,
{
    fn into_tls_websocket(self, url: &str) -> Websocket<TlsStream<Self>>
    where
        Self: Sized,
    {
        let url_tmp = Url::parse(url).unwrap();
        let server_name = url_tmp.host_str().unwrap();
        let tls_stream = self.into_tls_stream(server_name);
        Websocket::new(tls_stream, url).unwrap()
    }
}

pub trait TryIntoTlsReadyWebsocket {
    fn try_into_tls_ready_websocket(self) -> io::Result<Websocket<TlsReadyStream<TcpStream>>>
    where
        Self: Sized;
}

impl<T> TryIntoTlsReadyWebsocket for T
where
    T: AsRef<str>,
{
    fn try_into_tls_ready_websocket(self) -> io::Result<Websocket<TlsReadyStream<TcpStream>>>
    where
        Self: Sized,
    {
        let url = Url::parse(self.as_ref()).map_err(io::Error::other)?;
        let stream = TcpStream::connect(url.socket_addrs(|| None)?[0])?;

        let tls_ready_stream = match url.scheme() {
            "ws" => Ok(TlsReadyStream::Plain(stream)),
            "wss" => Ok(TlsReadyStream::Tls(TlsStream::wrap(stream, url.host_str().unwrap()))),
            scheme => Err(io::Error::other(format!("unrecognised url scheme: {}", scheme))),
        }?;

        Websocket::new(tls_ready_stream, self.as_ref())
    }
}

impl<'a, S> Websocket<S>
where
    S: Read + Write,
{
    pub fn new(mut stream: S, url: &'a str) -> io::Result<Self> {
        let mut handshaker = Handshaker::new();
        handshaker.send_handshake_request(&mut stream, url)?;

        Ok(Self {
            stream,
            handshaker,
            frame: Decoder::new(),
            closed: false,
            pending_pong: false,
            pong_payload: Vec::with_capacity(4096),
        })
    }
}
