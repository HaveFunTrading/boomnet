//! OS specific socket event notification mechanisms like `epoll`.

use std::io;

pub mod direct;
#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub mod io_uring;
#[cfg(feature = "mio")]
pub mod mio;

/// Identifies one connection incarnation to a selector. Treat the value as opaque.
/// Reconnection uses a fresh token even though the application's registration handle is unchanged.
pub type SelectorToken = u64;

/// Mutable lookup of active endpoints, excluding pending or removed registrations and stale tokens.
pub trait ActiveEndpointLookup<E> {
    /// Return the endpoint for this connection token, or `None` if it is no longer active.
    fn get_active_mut(&mut self, token: SelectorToken) -> Option<&mut E>;
}

pub trait Selectable {
    fn connected(&mut self) -> io::Result<bool>;

    fn make_writable(&mut self) -> io::Result<()>;

    fn make_readable(&mut self) -> io::Result<()>;
}

/// Drives readiness for endpoints owned by the caller. Tokens are allocated by the service.
pub trait Selector {
    type Target: Selectable;

    /// Register an endpoint under a fresh token that is never reused for another connection.
    fn register(&mut self, token: SelectorToken, endpoint: &mut Self::Target) -> io::Result<()>;

    /// Stop observing this token before the caller drops or replaces the endpoint.
    fn unregister(&mut self, token: SelectorToken, endpoint: &mut Self::Target) -> io::Result<()>;

    /// Apply readiness notifications through the active-endpoint lookup. Ignore stale tokens.
    fn poll(&mut self, endpoints: &mut impl ActiveEndpointLookup<Self::Target>) -> io::Result<()>;
}
