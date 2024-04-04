//! Entry point for the application logic.

use std::fmt::{Display, Formatter};
use std::io;
use std::net::SocketAddr;

use url::{ParseError, Url};

pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
}

impl Display for ConnectionInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl TryFrom<Url> for ConnectionInfo {
    type Error = io::Error;

    fn try_from(url: Url) -> Result<Self, Self::Error> {
        Ok(ConnectionInfo {
            host: url
                .host_str()
                .ok_or_else(|| io::Error::other("host not present"))?
                .to_owned(),
            port: url
                .port_or_known_default()
                .ok_or_else(|| io::Error::other("port not present"))?,
        })
    }
}

impl TryFrom<Result<Url, ParseError>> for ConnectionInfo {
    type Error = io::Error;

    fn try_from(result: Result<Url, ParseError>) -> Result<Self, Self::Error> {
        match result {
            Ok(url) => Ok(url.try_into()?),
            Err(err) => Err(io::Error::other(err)),
        }
    }
}

/// Entry point for the application logic. Endpoints are registered and Managed by 'IOService'.
pub trait Endpoint {
    /// Defines protocol and stream this endpoint operates on.
    type Target;

    /// Used by the `IOService` to obtain connection info from the endpoint.
    fn connection_info(&self) -> io::Result<ConnectionInfo>;

    /// Used by the `IOService` to create connection upon disconnect.
    fn create_target(&self, addr: SocketAddr) -> io::Result<Self::Target>;

    /// Called by the `IOService` on each duty cycle.
    fn poll(&mut self, target: &mut Self::Target) -> io::Result<()>;

    /// Upon disconnection `IOService` will query the endpoint if the connection can be
    /// recreated. If not it will cause panic.
    fn can_recreate(&mut self) -> bool {
        true
    }
}

/// Marker trait to be applied on user defined `struct` that is registered with 'IOService'
/// as context.
pub trait Context {}

/// Entry point for the application logic that exposes user provided [Context].
/// Endpoints are registered and Managed by `IOService`.
pub trait EndpointWithContext<C> {
    /// Defines protocol and stream this endpoint operates on.
    type Target;

    /// Used by the `IOService` to obtain connection info from the endpoint.
    fn connection_info(&self) -> io::Result<ConnectionInfo>;

    /// Used by the `IOService` to create connection upon disconnect passing user provided
    /// `Context`
    fn create_target(&self, addr: SocketAddr, context: &mut C) -> io::Result<Self::Target>;

    /// Called by the `IOService` on each duty cycle passing user provided `Context`.
    fn poll(&mut self, target: &mut Self::Target, context: &mut C) -> io::Result<()>;

    /// Upon disconnection `IOService` will query the endpoint if the connection can be
    /// recreated. If not, it will cause panic.
    fn can_recreate(&mut self) -> bool {
        true
    }
}

#[cfg(all(feature = "ws", feature = "tls"))]
pub mod ws {
    use std::io;
    use std::io::{Read, Write};
    use std::net::SocketAddr;

    use url::Url;

    use crate::endpoint::{ConnectionInfo, Endpoint, EndpointWithContext};
    use crate::stream::tls::TlsStream;
    use crate::ws::Websocket;

    pub type TlsWebsocket<S> = Websocket<TlsStream<S>>;

    pub trait TlsWebsocketEndpoint {
        type Stream: Read + Write;

        fn url(&self) -> &str;

        fn create_websocket(&self, addr: SocketAddr) -> io::Result<Websocket<TlsStream<Self::Stream>>>;

        fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>) -> io::Result<()>;

        fn can_recreate(&mut self) -> bool {
            true
        }
    }

    impl<T> Endpoint for T
    where
        T: TlsWebsocketEndpoint,
    {
        type Target = Websocket<TlsStream<T::Stream>>;

        #[inline]
        fn connection_info(&self) -> io::Result<ConnectionInfo> {
            Url::parse(self.url()).try_into()
        }

        #[inline]
        fn create_target(&self, addr: SocketAddr) -> io::Result<Self::Target> {
            self.create_websocket(addr)
        }

        #[inline]
        fn poll(&mut self, target: &mut Self::Target) -> io::Result<()> {
            self.poll(target)
        }

        #[inline]
        fn can_recreate(&mut self) -> bool {
            self.can_recreate()
        }
    }

    pub trait TlsWebsocketEndpointWithContext<C> {
        type Stream: Read + Write;

        fn url(&self) -> &str;

        fn create_websocket(&self, addr: SocketAddr, ctx: &mut C) -> io::Result<Websocket<TlsStream<Self::Stream>>>;

        fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>, ctx: &mut C) -> io::Result<()>;

        fn can_recreate(&mut self) -> bool {
            true
        }
    }

    impl<T, C> EndpointWithContext<C> for T
    where
        T: TlsWebsocketEndpointWithContext<C>,
    {
        type Target = Websocket<TlsStream<T::Stream>>;

        #[inline]
        fn connection_info(&self) -> io::Result<ConnectionInfo> {
            Url::parse(self.url()).try_into()
        }

        #[inline]
        fn create_target(&self, addr: SocketAddr, context: &mut C) -> io::Result<Self::Target> {
            self.create_websocket(addr, context)
        }

        #[inline]
        fn poll(&mut self, target: &mut Self::Target, context: &mut C) -> io::Result<()> {
            self.poll(target, context)
        }

        #[inline]
        fn can_recreate(&mut self) -> bool {
            self.can_recreate()
        }
    }
}
