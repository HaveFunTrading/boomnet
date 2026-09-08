//! OS specific socket event notification mechanisms like `epoll`.

use crate::service::node::{IONode, IONodes};
use std::io;

pub mod direct;
#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub mod io_uring;
#[cfg(feature = "mio")]
pub mod mio;

/// Used to uniquely identify a socket (connection) by the `Selector`.
pub type SelectorToken = u32;

pub trait Selectable {
    fn connected(&mut self) -> io::Result<bool>;

    fn make_writable(&mut self) -> io::Result<()>;

    fn make_readable(&mut self) -> io::Result<()>;
}

pub trait Selector {
    type Target: Selectable;

    fn register<F>(&mut self, selector_token: SelectorToken, io_node: &mut IONode<Self::Target, F>) -> io::Result<()>;

    fn unregister<F>(&mut self, io_node: &mut IONode<Self::Target, F>) -> io::Result<()>;

    fn poll<F>(&mut self, io_nodes: &mut IONodes<Self::Target, F>) -> io::Result<()>;

    fn next_token(&mut self) -> SelectorToken;
}
