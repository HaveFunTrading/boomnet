//! Entry point for the application logic.

use std::fmt::Display;
use std::io;
use std::net::SocketAddr;

use crate::stream::ConnectionInfoProvider;

/// Entry point for the application logic. Endpoints are registered and Managed by 'IOService'.
pub trait Endpoint: ConnectionInfoProvider {
    /// Defines protocol and stream this endpoint operates on.
    type Target;

    /// Used by the `IOService` to create connection upon disconnect.
    fn create_target(&mut self, addr: SocketAddr) -> io::Result<Self::Target>;

    /// Called by the `IOService` on each duty cycle.
    fn poll(&mut self, target: &mut Self::Target) -> io::Result<()>;

    /// Upon disconnection `IOService` will query the endpoint if the connection can be
    /// recreated. If not, it will cause program to panic.
    fn can_recreate(&mut self) -> bool {
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

    /// Used by the `IOService` to create connection upon disconnect passing user provided
    /// `Context`
    fn create_target(&mut self, addr: SocketAddr, context: &mut C) -> io::Result<Self::Target>;

    /// Called by the `IOService` on each duty cycle passing user provided `Context`.
    fn poll(&mut self, target: &mut Self::Target, context: &mut C) -> io::Result<()>;

    /// Upon disconnection `IOService` will query the endpoint if the connection can be
    /// recreated. If not, it will cause program to panic.
    fn can_recreate(&mut self, _context: &mut C) -> bool {
        true
    }

    /// When `auto_disconnect` is used the service will check with the endpoint before
    /// disconnecting. If `false` is returned the service will update the endpoint next
    /// disconnect time as per the `auto_disconnect` configuration.
    fn can_auto_disconnect(&mut self, _context: &mut C) -> bool {
        true
    }
}

#[cfg(all(feature = "ws", any(feature = "tls-webpki", feature = "tls-native")))]
pub mod ws {
    use std::io;
    use std::io::{Read, Write};
    use std::net::SocketAddr;

    use crate::service::endpoint::{Endpoint, EndpointWithContext};
    use crate::stream::tls::TlsStream;
    use crate::stream::ConnectionInfoProvider;
    use crate::ws::Websocket;

    pub type TlsWebsocket<S> = Websocket<TlsStream<S>>;

    pub trait TlsWebsocketEndpoint: ConnectionInfoProvider {
        type Stream: Read + Write;

        fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<Websocket<TlsStream<Self::Stream>>>;

        fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>) -> io::Result<()>;

        fn can_recreate(&mut self) -> bool {
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
        fn create_target(&mut self, addr: SocketAddr) -> io::Result<Self::Target> {
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

        #[inline]
        fn can_auto_disconnect(&mut self) -> bool {
            self.can_auto_disconnect()
        }
    }

    pub trait TlsWebsocketEndpointWithContext<C>: ConnectionInfoProvider {
        type Stream: Read + Write;

        fn create_websocket(&mut self, addr: SocketAddr, ctx: &mut C)
            -> io::Result<Websocket<TlsStream<Self::Stream>>>;

        fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>, ctx: &mut C) -> io::Result<()>;

        fn can_recreate(&mut self, _ctx: &mut C) -> bool {
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
        fn create_target(&mut self, addr: SocketAddr, context: &mut C) -> io::Result<Self::Target> {
            self.create_websocket(addr, context)
        }

        #[inline]
        fn poll(&mut self, target: &mut Self::Target, context: &mut C) -> io::Result<()> {
            self.poll(target, context)
        }

        #[inline]
        fn can_recreate(&mut self, context: &mut C) -> bool {
            self.can_recreate(context)
        }

        #[inline]
        fn can_auto_disconnect(&mut self, context: &mut C) -> bool {
            self.can_auto_disconnect(context)
        }
    }
}
