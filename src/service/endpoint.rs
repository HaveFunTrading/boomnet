//! Endpoint creation and lifecycle policy.

use crate::stream::ConnectionInfoProvider;
use std::fmt::{Debug, Display};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

/// Creates I/O endpoints and defines their lifecycle policy for [`crate::service::IOService`].
///
/// Register the factory with the service. It retains the factory across reconnects and uses
/// it to create a replacement endpoint whenever recreation is allowed.
pub trait EndpointFactory: ConnectionInfoProvider {
    /// The created I/O object, such as a WebSocket over a TLS stream.
    type Endpoint;

    /// Shared state borrowed during lifecycle callbacks. Use `()` when no context is needed.
    type Context;

    /// Create an endpoint using the resolved address, on initial connection or reconnection.
    /// Return `Ok(None)` to defer creation until a later attempt, possibly with a different address.
    fn create_endpoint(&mut self, addr: SocketAddr, ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>>;

    /// Decide whether the service should recreate a disconnected endpoint.
    /// Returning `false` makes the service return
    /// [`crate::service::IOServiceError::EndpointNotRecreatable`].
    fn can_recreate(&mut self, _reason: &DisconnectReason, _ctx: &mut Self::Context) -> bool {
        true
    }

    /// Decide whether an endpoint whose configured TTL has expired may be disconnected.
    /// Returning `false` extends its lifetime using the service's `auto_disconnect` configuration.
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
