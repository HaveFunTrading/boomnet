//! Entry point for the application logic.

use crate::stream::ConnectionInfoProvider;
use std::fmt::{Debug, Display};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

/// Describes how an I/O target is created and recreated by [`crate::service::IOService`].
pub trait Endpoint: ConnectionInfoProvider {
    /// Defines protocol and stream this endpoint operates on.
    type Target;

    /// Shared state borrowed during lifecycle callbacks. Use `()` when no context is needed.
    type Context;

    /// Used by the `IOService` to create connection upon disconnect by passing resolved `addr`.
    /// If the endpoint does not want to connect at this stage it should return `Ok(None)` and
    /// await the next connection attempt with (possibly) different `addr`.
    fn create_target(&mut self, addr: SocketAddr, ctx: &mut Self::Context) -> io::Result<Option<Self::Target>>;

    /// Upon disconnection `IOService` will query the endpoint if the connection should be
    /// recreated, passing the disconnect `reason`. Returning `false` makes the service return
    /// [`crate::service::IOServiceError::EndpointNotRecreatable`].
    fn can_recreate(&mut self, _reason: &DisconnectReason, _ctx: &mut Self::Context) -> bool {
        true
    }

    /// When `auto_disconnect` is used the service will check with the endpoint before
    /// disconnecting. If `false` is returned the service will update the endpoint next
    /// disconnect time as per the `auto_disconnect` configuration.
    fn can_auto_disconnect(&mut self, _ctx: &mut Self::Context) -> bool {
        true
    }
}

/// Disconnect reason passed into `can_recreate()` service call.
#[derive(Debug)]
pub enum DisconnectReason {
    /// This is expected disconnection due to `ttl` on the connection expiring.
    AutoDisconnect(Duration),
    /// IO error has occurred such as reaching EOF or peer disconnect.
    IO(io::Error),
}

impl Display for DisconnectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisconnectReason::AutoDisconnect(ttl) => {
                write!(f, "auto-disconnect after ")?;
                ttl.fmt(f)
            }
            DisconnectReason::IO(err) => {
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
        DisconnectReason::IO(err)
    }
}
