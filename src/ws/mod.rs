//! Websocket client protocol implementation.
//!
//! ## Examples
//!
//! Create a TLS websocket from a stream.
//! ```no_run
//! use std::net::TcpStream;
//! use boomnet::stream::BindAndConnect;
//! use boomnet::stream::buffer::IntoBufferedStream;
//! use boomnet::stream::tls::IntoTlsStream;
//! use boomnet::ws::IntoWebsocket;
//!
//! let mut ws = TcpStream::bind_and_connect("stream.binance.com:9443", None, None).unwrap()
//! .into_tls_stream("stream.binance.com")
//! .into_default_buffered_stream()
//! .into_websocket("wss://stream.binance.com:9443/ws");
//! ```
//!
//! Quickly create websocket from a valid url (for debugging purposes only).
//! ```no_run
//! use boomnet::ws::TryIntoTlsReadyWebsocket;
//!
//! let mut ws = "wss://stream.binance.com:443/ws".try_into_tls_ready_websocket().unwrap();
//! ```
//!
//! Receive messages in a batch for optimal performance.
//! ```no_run
//! use std::io::{Read, Write};
//! use boomnet::ws::{Websocket, WebsocketFrame};
//!
//! fn consume_batch<S: Read + Write>(ws: &mut Websocket<S>) {
//!    for frame in ws.batch_iter().unwrap() {
//!      if let WebsocketFrame::Text(fin, body) = frame.unwrap() {
//!        println!("({fin}) {}", String::from_utf8_lossy(body));
//!      }
//!    }
//! }
//! ```
//!
//! Receive messages at most one at a tine. If possible, use batch mode instead.
//!```no_run
//! use std::io::{Read, Write};
//! use boomnet::ws::{Websocket, WebsocketFrame};
//!
//! fn consume_individually<S: Read + Write>(ws: &mut Websocket<S>) {
//!   if let Some(WebsocketFrame::Text(fin, body)) = ws.receive_next().unwrap() {
//!     println!("({fin}) {}", String::from_utf8_lossy(body));
//!   }
//! }
//! ```

#[cfg(feature = "mio")]
use mio::{event::Source, Interest, Registry, Token};
use std::io;
use std::io::ErrorKind::WouldBlock;
use std::io::{Read, Write};
use std::net::TcpStream;
use thiserror::Error;
use url::Url;

use crate::buffer;
use crate::select::Selectable;
#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
use crate::stream::tls::{IntoTlsStream, NotTlsStream, TlsReadyStream, TlsStream};
use crate::util::NoBlock;
use crate::ws::decoder::Decoder;
use crate::ws::handshake::Handshaker;
use crate::ws::Error::{Closed, ReceivedCloseFrame};

// re-export
pub use crate::ws::error::Error;

mod decoder;
pub mod ds;
mod encoder;
mod error;
mod handshake;
mod protocol;

type ReadBuffer = buffer::ReadBuffer<4096>;

/// Supported web socket frame variants.
pub enum WebsocketFrame {
    /// Server has sent ping frame that will generate automatic pong response. This frame is not
    /// exposed to the user.
    Ping(&'static [u8]),
    Pong(&'static [u8]),
    Text(bool, &'static [u8]),
    Binary(bool, &'static [u8]),
    Continuation(bool, &'static [u8]),
    /// Server has sent close frame. The websocket will be closed as a result. This frame is not
    /// exposed to the user.
    Close(&'static [u8]),
}

/// Websocket client that owns underlying stream.
#[derive(Debug)]
pub struct Websocket<S> {
    stream: S,
    closed: bool,
    state: State,
}

impl<S> Websocket<S> {
    /// Checks if the websocket is closed. This can be result of an IO error or the other side
    /// sending `WebsocketFrame::Closed`.
    pub const fn closed(&self) -> bool {
        self.closed
    }

    /// Checks if the handshake has completed successfully. If attempt is made to send a message
    /// while the handshake is pending the message will be buffered and dispatched once handshake
    /// has finished.
    #[inline]
    pub const fn handshake_complete(&self) -> bool {
        match self.state {
            State::Handshake(_) => false,
            State::Connection(_) => true,
        }
    }

    fn new(stream: S, url: &str) -> io::Result<Self> {
        Ok(Self {
            stream,
            closed: false,
            state: State::handshake(url)?,
        })
    }
}

impl<S: Read + Write> Websocket<S> {
    #[inline]
    pub fn batch_iter(&mut self) -> Result<BatchIter<S>, Error> {
        match self.state.read(&mut self.stream).no_block() {
            Ok(()) => Ok(BatchIter { websocket: self }),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    pub fn receive_next(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        self.batch_iter()?.next().transpose()
    }

    #[inline]
    pub fn send_text(&mut self, fin: bool, body: Option<&[u8]>) -> Result<(), Error> {
        self.send(fin, protocol::op::TEXT_FRAME, body)
    }

    #[inline]
    pub fn send_binary(&mut self, fin: bool, body: Option<&[u8]>) -> Result<(), Error> {
        self.send(fin, protocol::op::BINARY_FRAME, body)
    }

    #[inline]
    pub fn send_pong(&mut self, body: Option<&[u8]>) -> Result<(), Error> {
        self.send(true, protocol::op::PONG, body)
    }

    #[inline]
    pub fn send_ping(&mut self, body: Option<&[u8]>) -> Result<(), Error> {
        self.send(true, protocol::op::PING, body)
    }

    #[inline]
    fn next(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        self.ensure_not_closed()?;
        match self.state.next(&mut self.stream) {
            Ok(frame) => Ok(frame),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    fn send(&mut self, fin: bool, op_code: u8, body: Option<&[u8]>) -> Result<(), Error> {
        self.ensure_not_closed()?;
        match self.state.send(&mut self.stream, fin, op_code, body) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    const fn ensure_not_closed(&self) -> Result<(), Error> {
        if self.closed {
            return Err(Closed);
        }
        Ok(())
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

#[derive(Debug)]
enum State {
    Handshake(Handshaker),
    Connection(Decoder),
}

impl State {
    pub fn handshake(url: &str) -> Result<Self, Error> {
        Ok(Self::Handshake(Handshaker::new(url)?))
    }

    pub fn connection() -> Self {
        Self::Connection(Decoder::new())
    }
}

impl State {
    #[inline]
    fn read<S: Read>(&mut self, stream: &mut S) -> io::Result<()> {
        match self {
            State::Handshake(handshake) => handshake.read(stream),
            State::Connection(decoder) => decoder.read(stream),
        }
    }

    #[inline]
    fn next<S: Read + Write>(&mut self, stream: &mut S) -> Result<Option<WebsocketFrame>, Error> {
        match self {
            State::Handshake(handshake) => match handshake.perform_handshake(stream) {
                Ok(()) => {
                    handshake.drain_pending_message_buffer(stream, encoder::send)?;
                    *self = State::connection();
                    Ok(None)
                }
                Err(err) if err.kind() == WouldBlock => Ok(None),
                Err(err) => Err(err)?,
            },
            State::Connection(decoder) => match decoder.decode_next() {
                Ok(Some(WebsocketFrame::Ping(payload))) => {
                    self.send(stream, true, protocol::op::PONG, Some(payload))?;
                    Ok(None)
                }
                Ok(Some(WebsocketFrame::Close(payload))) => {
                    let _ = self.send(stream, true, protocol::op::CONNECTION_CLOSE, Some(payload));
                    let (status_code, body) = payload.split_at(std::mem::size_of::<u16>());
                    let status_code = u16::from_be_bytes(status_code.try_into()?);
                    let body = String::from_utf8_lossy(body).to_string();
                    Err(ReceivedCloseFrame(status_code, body))
                }
                Ok(frame) => Ok(frame),
                Err(err) => Err(err)?,
            },
        }
    }

    #[inline]
    fn send<S: Write>(&mut self, stream: &mut S, fin: bool, op_code: u8, body: Option<&[u8]>) -> Result<(), Error> {
        match self {
            State::Handshake(handshake) => {
                handshake.buffer_message(fin, op_code, body);
                Ok(())
            }
            State::Connection(_) => {
                encoder::send(stream, fin, op_code, body)?;
                Ok(())
            }
        }
    }
}

pub struct BatchIter<'a, S> {
    websocket: &'a mut Websocket<S>,
}

impl<S: Read + Write> Iterator for BatchIter<'_, S> {
    type Item = Result<WebsocketFrame, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.websocket.next().transpose()
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
