//! Websocket client protocol implementation.
//!
//! ## Examples
//!
//! Create a TLS websocket from a stream.
//!```no_run
//! use std::net::TcpStream;
//! use boomnet::stream::{BindAndConnect, ConnectionInfo};
//! use boomnet::stream::buffer::IntoBufferedStream;
//! use boomnet::stream::tls::IntoTlsStream;
//! use boomnet::ws::IntoWebsocket;
//!
//! let mut ws = ConnectionInfo::new("stream.binance.com", 9443)
//! .into_tcp_stream().unwrap()
//! .into_tls_stream()
//! .into_default_buffered_stream()
//! .into_websocket("/ws");
//! ```
//!
//! Quickly create websocket from a valid url (for debugging purposes only).
//! ```no_run
//! use boomnet::ws::TryIntoTlsReadyWebsocket;
//!
//! let mut ws = "wss://stream.binance.com/ws".try_into_tls_ready_websocket().unwrap();
//! ```
//!
//! Receive messages in a batch for optimal performance.
//!```no_run
//! use std::io::{Read, Write};
//! use boomnet::ws::{Websocket, WebsocketFrame};
//!
//! fn consume_batch<S: Read + Write>(ws: &mut Websocket<S>) -> std::io::Result<()> {
//!    for frame in ws.read_batch()? {
//!      if let WebsocketFrame::Text(fin, body) = frame? {
//!        println!("({fin}) {}", String::from_utf8_lossy(body));
//!      }
//!    }
//!    Ok(())
//! }
//! ```
//!
//! Receive messages at most one at a tine. If possible, use batch mode instead.
//!```no_run
//! use std::io::{Read, Write};
//! use boomnet::ws::{Websocket, WebsocketFrame};
//!
//! fn consume_individually<S: Read + Write>(ws: &mut Websocket<S>) -> std::io::Result<()> {
//!   if let Some(frame) = ws.receive_next() {
//!     if let WebsocketFrame::Text(fin, body) = frame? {
//!       println!("({fin}) {}", String::from_utf8_lossy(body));
//!     }
//!   }
//!   Ok(())
//! }
//! ```

use std::fmt::Debug;
use crate::buffer;
use crate::service::select::Selectable;
#[cfg(any(feature = "rustls", feature = "openssl"))]
use crate::stream::tls::{IntoTlsStream, TlsReadyStream, TlsStream};
use crate::stream::{BindAndConnect, ConnectionInfoProvider};
use crate::util::NoBlock;
use crate::ws::decoder::Decoder;
use crate::ws::handshake::Handshaker;
use crate::ws::Error::{Closed, ReceivedCloseFrame};
#[cfg(feature = "mio")]
use mio::{event::Source, Interest, Registry, Token};
use std::io;
use std::io::ErrorKind::WouldBlock;
use std::io::{Read, Write};
use thiserror::Error;
use url::Url;

// re-export
pub use crate::ws::error::Error;

mod decoder;
pub mod ds;
mod encoder;
mod error;
mod handshake;
mod protocol;
pub mod util;

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
    pub fn new(stream: S, server_name: &str, endpoint: &str) -> Websocket<S> {
        Self {
            stream,
            closed: false,
            state: State::handshake(server_name, endpoint),
        }
    }

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
}

impl<S: Read + Write> Websocket<S> {
    /// Allows to decode and iterate over incoming messages in a batch efficient way. It will perform
    /// single network read operation if there is no more data available for processing. It is possible
    /// to receive more than one message from a single network read and when no messages are available
    /// in the current batch, the iterator will yield `None`.
    ///
    /// ## Examples
    ///
    /// Process incoming frames in a batch using iterator,
    /// ```no_run
    /// use std::io::{Read, Write};
    /// use boomnet::ws::{Websocket, WebsocketFrame};
    ///
    /// fn process<S: Read + Write>(ws: &mut Websocket<S>) -> std::io::Result<()> {
    ///     for frame in ws.read_batch()? {
    ///         if let (WebsocketFrame::Text(fin, data)) = frame? {
    ///             println!("({fin}) {}", String::from_utf8_lossy(data));
    ///         }
    ///     }
    ///     Ok(())
    /// }
    /// ```
    ///
    /// Read frames one by one without iterator,
    /// ```no_run
    /// use std::io::{Read, Write};
    /// use boomnet::ws::{Websocket, WebsocketFrame};
    ///
    /// fn process<S: Read + Write>(ws: &mut Websocket<S>) -> std::io::Result<()> {
    ///     let mut batch = ws.read_batch()?;
    ///     while let Some(frame) = batch.receive_next() {
    ///         if let (WebsocketFrame::Text(fin, data)) = frame? {
    ///             println!("({fin}) {}", String::from_utf8_lossy(data));
    ///         }
    ///     }
    ///     Ok(())
    /// }
    /// ```
    #[inline]
    pub fn read_batch(&mut self) -> Result<Batch<S>, Error> {
        match self.state.read(&mut self.stream).no_block() {
            Ok(()) => Ok(Batch { websocket: self }),
            Err(err) => {
                self.closed = true;
                Err(err)?
            }
        }
    }

    #[inline]
    pub fn receive_next(&mut self) -> Option<Result<WebsocketFrame, Error>> {
        match self.read_batch() {
            Ok(mut batch) => batch.receive_next(),
            Err(err) => Some(Err(err)),
        }
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
    pub fn handshake(server_name: &str, endpoint: &str) -> Self {
        Self::Handshake(Handshaker::new(server_name, endpoint))
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

/// Represents a batch of 0 to N websocket frames since the last network read that are ready to be decoded.
pub struct Batch<'a, S> {
    websocket: &'a mut Websocket<S>,
}

impl<'a, S: Read + Write> IntoIterator for Batch<'a, S> {
    type Item = Result<WebsocketFrame, Error>;
    type IntoIter = BatchIter<'a, S>;

    fn into_iter(self) -> Self::IntoIter {
        BatchIter { batch: self }
    }
}

impl<S: Read + Write> Batch<'_, S> {
    /// Try to decode next frame from the underlying `Batch`. If no more frames are available it
    /// will return `None`.
    pub fn receive_next(&mut self) -> Option<Result<WebsocketFrame, Error>> {
        self.websocket.next().transpose()
    }
}

/// Iterator that owns the current `Batch`. When no more frames are available to be decoded in the buffer
/// it will yield `None`.
pub struct BatchIter<'a, S> {
    batch: Batch<'a, S>,
}

impl<S: Read + Write> Iterator for BatchIter<'_, S> {
    type Item = Result<WebsocketFrame, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.batch.receive_next()
    }
}

pub trait IntoWebsocket {
    fn into_websocket(self, endpoint: &str) -> Websocket<Self>
    where
        Self: Sized;
}

impl<T> IntoWebsocket for T
where
    T: Read + Write + ConnectionInfoProvider,
{
    fn into_websocket(self, endpoint: &str) -> Websocket<Self>
    where
        Self: Sized,
    {
        let host = self.connection_info().host().to_owned();
        Websocket::new(self, &host, endpoint)
    }
}

#[cfg(any(feature = "rustls", feature = "openssl"))]
pub trait IntoTlsWebsocket {
    fn into_tls_websocket(self, endpoint: &str) -> io::Result<Websocket<TlsStream<Self>>>
    where
        Self: Sized;
}

#[cfg(any(feature = "rustls", feature = "openssl"))]
impl<T> IntoTlsWebsocket for T
where
    T: Read + Write + Debug + ConnectionInfoProvider,
{
    fn into_tls_websocket(self, endpoint: &str) -> io::Result<Websocket<TlsStream<Self>>>
    where
        Self: Sized,
    {
        Ok(self.into_tls_stream()?.into_websocket(endpoint))
    }
}

#[cfg(any(feature = "rustls", feature = "openssl"))]
pub trait TryIntoTlsReadyWebsocket {
    fn try_into_tls_ready_websocket(self) -> io::Result<Websocket<TlsReadyStream<std::net::TcpStream>>>
    where
        Self: Sized;
}

#[cfg(any(feature = "rustls", feature = "openssl"))]
impl<T> TryIntoTlsReadyWebsocket for T
where
    T: AsRef<str>,
{
    fn try_into_tls_ready_websocket(self) -> io::Result<Websocket<TlsReadyStream<std::net::TcpStream>>>
    where
        Self: Sized,
    {
        let url = Url::parse(self.as_ref()).map_err(io::Error::other)?;

        let addr = url.socket_addrs(|| match url.scheme() {
            "ws" => Some(80),
            "wss" => Some(443),
            _ => None,
        })?;

        let endpoint = match url.query() {
            Some(query) => format!("{}?{}", url.path(), query),
            None => url.path().to_string(),
        };

        let stream = std::net::TcpStream::bind_and_connect(addr[0], None, None)?;

        let tls_ready_stream = match url.scheme() {
            "ws" => Ok(TlsReadyStream::Plain(stream)),
            "wss" => Ok(TlsReadyStream::Tls(TlsStream::wrap(stream, url.host_str().unwrap()).unwrap())),
            scheme => Err(io::Error::other(format!("unrecognised url scheme: {}", scheme))),
        }?;

        Ok(Websocket::new(tls_ready_stream, url.host_str().unwrap(), &endpoint))
    }
}
