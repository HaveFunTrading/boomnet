//! Entry point for the application logic.

use crate::stream::ConnectionInfoProvider;
use std::fmt::{Debug, Display};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

/// Entry point for the application logic. Endpoints are registered and Managed by 'IOService'.
pub trait Endpoint: ConnectionInfoProvider {
    /// Defines protocol and stream this endpoint operates on.
    type Target;

    /// Used by the `IOService` to create connection upon disconnect by passing resolved `addr`.
    /// If the endpoint does not want to connect at this stage it should return `Ok(None)` and
    /// await the next connection attempt with (possibly) different `addr`.
    fn create_target(&mut self, addr: SocketAddr) -> io::Result<Option<Self::Target>>;

    /// Called by the `IOService` on each duty cycle.
    fn poll(&mut self, target: &mut Self::Target) -> io::Result<()>;

    /// Upon disconnection `IOService` will query the endpoint if the connection can be
    /// recreated, passing the disconnect `reason`. If not, it will cause program to panic.
    fn can_recreate(&mut self, _reason: DisconnectReason) -> bool {
        true
    }

    /// When `auto_disconnect` is used the service will check with the endpoint before
    /// disconnecting. If `false` is returned the service will update the endpoint next
    /// disconnect time as per the `auto_disconnect` configuration.
    fn can_auto_disconnect(&mut self) -> bool {
        true
    }
}

/// Marker trait to be applied on user defined `struct` that is registered with 'IOService'
/// as context.
pub trait Context {}

/// Entry point for the application logic that exposes user provided [Context].
/// Endpoints are registered and Managed by `IOService`.
pub trait EndpointWithContext<C>: ConnectionInfoProvider {
    /// Defines protocol and stream this endpoint operates on.
    type Target;

    /// Used by the `IOService` to create connection upon disconnect passing resolved `addr` and
    /// user provided `Context`. If the endpoint does not want to connect at this stage it should
    /// return `Ok(None)` and await the next connection attempt with (possibly) different `addr`.
    fn create_target(&mut self, addr: SocketAddr, context: &mut C) -> io::Result<Option<Self::Target>>;

    /// Called by the `IOService` on each duty cycle passing user provided `Context`.
    fn poll(&mut self, target: &mut Self::Target, context: &mut C) -> io::Result<()>;

    /// Upon disconnection `IOService` will query the endpoint if the connection can be
    /// recreated, passing the disconnect `reason`. If not, it will cause program to panic.
    fn can_recreate(&mut self, _reason: DisconnectReason, _context: &mut C) -> bool {
        true
    }

    /// When `auto_disconnect` is used the service will check with the endpoint before
    /// disconnecting. If `false` is returned the service will update the endpoint next
    /// disconnect time as per the `auto_disconnect` configuration.
    fn can_auto_disconnect(&mut self, _context: &mut C) -> bool {
        true
    }
}

/// Disconnect reason passed into `can_recreate()` service call.
pub enum DisconnectReason {
    /// This is expected disconnection due to `ttl` on the connection expiring.
    AutoDisconnect(Duration),
    /// Some other IO error has occurred such as reaching EOF or peer disconnect. It's normally
    /// ok to try and connect again.
    Other(io::Error),
}

impl Display for DisconnectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisconnectReason::AutoDisconnect(ttl) => {
                write!(f, "auto-disconnect after ")?;
                ttl.fmt(f)
            }
            DisconnectReason::Other(err) => {
                write!(f, "{err}")
            }
        }
    }
}

impl DisconnectReason {
    pub(crate) fn auto_disconnect(ttl: Duration) -> DisconnectReason {
        DisconnectReason::AutoDisconnect(ttl)
    }

    pub(crate) fn other(err: io::Error) -> DisconnectReason {
        DisconnectReason::Other(err)
    }
}

#[cfg(all(feature = "ext", feature = "ws", any(feature = "rustls", feature = "openssl")))]
pub mod ws {
    use std::io;
    use std::io::{Read, Write};
    use std::net::SocketAddr;

    use crate::service::endpoint::{DisconnectReason, Endpoint, EndpointWithContext};
    use crate::stream::ConnectionInfoProvider;
    use crate::stream::tls::TlsStream;
    use crate::ws::Websocket;

    pub type TlsWebsocket<S> = Websocket<TlsStream<S>>;

    pub trait TlsWebsocketEndpoint: ConnectionInfoProvider {
        type Stream: Read + Write;

        fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<Option<Websocket<TlsStream<Self::Stream>>>>;

        fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>) -> io::Result<()>;

        fn can_recreate(&mut self, _reason: DisconnectReason) -> bool {
            true
        }

        fn can_auto_disconnect(&mut self) -> bool {
            true
        }
    }

    impl<T> Endpoint for T
    where
        T: TlsWebsocketEndpoint,
    {
        type Target = Websocket<TlsStream<T::Stream>>;

        #[inline]
        fn create_target(&mut self, addr: SocketAddr) -> io::Result<Option<Self::Target>> {
            self.create_websocket(addr)
        }

        #[inline]
        fn poll(&mut self, target: &mut Self::Target) -> io::Result<()> {
            self.poll(target)
        }

        #[inline]
        fn can_recreate(&mut self, reason: DisconnectReason) -> bool {
            self.can_recreate(reason)
        }

        #[inline]
        fn can_auto_disconnect(&mut self) -> bool {
            self.can_auto_disconnect()
        }
    }

    pub trait TlsWebsocketEndpointWithContext<C>: ConnectionInfoProvider {
        type Stream: Read + Write;

        fn create_websocket(
            &mut self,
            addr: SocketAddr,
            ctx: &mut C,
        ) -> io::Result<Option<Websocket<TlsStream<Self::Stream>>>>;

        fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>, ctx: &mut C) -> io::Result<()>;

        fn can_recreate(&mut self, _reason: DisconnectReason, _ctx: &mut C) -> bool {
            true
        }

        fn can_auto_disconnect(&mut self, _ctx: &mut C) -> bool {
            true
        }
    }

    impl<T, C> EndpointWithContext<C> for T
    where
        T: TlsWebsocketEndpointWithContext<C>,
    {
        type Target = Websocket<TlsStream<T::Stream>>;

        #[inline]
        fn create_target(&mut self, addr: SocketAddr, context: &mut C) -> io::Result<Option<Self::Target>> {
            self.create_websocket(addr, context)
        }

        #[inline]
        fn poll(&mut self, target: &mut Self::Target, context: &mut C) -> io::Result<()> {
            self.poll(target, context)
        }

        #[inline]
        fn can_recreate(&mut self, reason: DisconnectReason, context: &mut C) -> bool {
            self.can_recreate(reason, context)
        }

        #[inline]
        fn can_auto_disconnect(&mut self, context: &mut C) -> bool {
            self.can_auto_disconnect(context)
        }
    }
}
