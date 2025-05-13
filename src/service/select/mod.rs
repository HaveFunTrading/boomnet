//! OS specific socket event notification mechanisms like `epoll`.

use crate::service::node::IONode;
use std::collections::HashMap;
use std::io;

pub mod direct;
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

    fn register<E>(&mut self, selector_token: SelectorToken, io_node: &mut IONode<Self::Target, E>) -> io::Result<()>;

    fn unregister<E>(&mut self, io_node: &mut IONode<Self::Target, E>) -> io::Result<()>;

    fn poll<E>(&mut self, io_nodes: &mut HashMap<SelectorToken, IONode<Self::Target, E>>) -> io::Result<()>;

    fn next_token(&mut self) -> SelectorToken;
}
