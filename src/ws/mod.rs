//! Websocket protocol.

#[cfg(feature = "mio")]
use mio::{event::Source, Interest, Registry, Token};
use std::collections::VecDeque;
use std::io;
use std::io::ErrorKind::{Other, WouldBlock};
use std::io::{Read, Write};
use std::net::TcpStream;
use thiserror::Error;
use url::{ParseError, Url};

use crate::buffer;
use crate::select::Selectable;
#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
use crate::stream::tls::{IntoTlsStream, NotTlsStream, TlsReadyStream, TlsStream};
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
    #[error("url parse error: {0}")]
    UrlParse(#[from] ParseError),
}

impl From<Error> for io::Error {
    fn from(value: Error) -> Self {
        io::Error::new(Other, value)
    }
}

pub enum WebsocketFrame {
    Ping(u64, &'static [u8]),
    Pong(u64, &'static [u8]),
    Text(u64, bool, &'static [u8]),
    Binary(u64, bool, &'static [u8]),
    Continuation(u64, bool, &'static [u8]),
    Close(u64, &'static [u8]),
}

#[derive(Debug)]
pub struct Websocket<S> {
    stream: S,
    handshaker: Handshaker,
    frame: Decoder,
    closed: bool,
    pending_msg_buffer: VecDeque<(u8, bool, Option<Vec<u8>>)>,
}

impl<S> Websocket<S> {
    /// Checks if the websocket is closed. THis can be result of an IO error or the other side
    /// sending `WebsocketFrame::Closed`.
    pub const fn closed(&self) -> bool {
        self.closed
    }

    /// Checks if the handshake has completed successfully. If attempt is made to send a message
    /// while the handshake is pending the message will be buffered and dispatched once handshake
    /// has finished.
    #[inline]
    pub const fn handshake_complete(&self) -> bool {
        matches!(self.handshaker.state(), HandshakeState::Completed)
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
    pub fn new(stream: S, url: &str) -> io::Result<Self> {
        Ok(Self {
            stream,
            handshaker: Handshaker::new(url)?,
            frame: Decoder::new(),
            closed: false,
            pending_msg_buffer: VecDeque::with_capacity(256),
        })
    }

    pub fn receive_next(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        self.ensure_not_closed()?;
        match self.handshaker.state() {
            HandshakeState::NotStarted => self.initiate_handshake(),
            HandshakeState::Pending => self.perform_handshake(),
            HandshakeState::Completed => self.decode_next_frame(),
        }
    }

    #[cold]
    fn initiate_handshake(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        self.handshaker.send_handshake_request(&mut self.stream)?;
        Ok(None)
    }

    #[cold]
    fn perform_handshake(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        match self.handshaker.await_handshake(&mut self.stream) {
            Ok(()) => {
                // drain pending message buffer
                while let Some((op, fin, body)) = self.pending_msg_buffer.pop_front() {
                    self.send(fin, op, body.as_deref())?;
                }
                Ok(None)
            }
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
                self.send_pong(Some(payload))?;
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
        if self.handshake_complete() {
            self.send(fin, protocol::op::TEXT_FRAME, body)?;
        } else {
            self.buffer_message(fin, protocol::op::TEXT_FRAME, body);
        }
        Ok(())
    }

    #[inline]
    pub fn send_binary(&mut self, fin: bool, body: Option<&[u8]>) -> Result<(), Error> {
        if self.handshake_complete() {
            self.send(fin, protocol::op::BINARY_FRAME, body)?;
        } else {
            self.buffer_message(fin, protocol::op::BINARY_FRAME, body);
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn buffer_message(&mut self, fin: bool, op: u8, body: Option<&[u8]>) {
        let body = body.map(|body| body.to_vec());
        self.pending_msg_buffer.push_back((op, fin, body))
    }

    #[inline]
    fn send_pong(&mut self, payload: Option<&[u8]>) -> Result<(), Error> {
        self.send(true, protocol::op::PONG, payload)
    }

    #[inline]
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
    const fn ensure_not_closed(&self) -> Result<(), Error> {
        #[cold]
        #[inline(never)]
        const fn signal_closed() -> Result<(), Error> {
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

#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
pub trait IntoTlsWebsocket {
    fn into_tls_websocket(self, url: &str) -> Websocket<TlsStream<Self>>
    where
        Self: Sized;
}

#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
impl<T> IntoTlsWebsocket for T
where
    T: Read + Write + NotTlsStream,
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

#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
pub trait TryIntoTlsReadyWebsocket {
    fn try_into_tls_ready_websocket(self) -> io::Result<Websocket<TlsReadyStream<TcpStream>>>
    where
        Self: Sized;
}

#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
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
