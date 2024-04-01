//! Service to manage multiple endpoint  lifecycle.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;
use std::{io, mem};

use log::error;

use crate::endpoint::{Context, Endpoint, EndpointWithContext};
use crate::idle::IdleStrategy;
use crate::node::IONode;
use crate::select::{Selector, SelectorToken};
use crate::util::current_time_nanos;

const ENDPOINT_CREATION_THROTTLE_NS: u64 = Duration::from_secs(1).as_nanos() as u64;

/// Handles the lifecycle of endpoints (see [`Endpoint`]), which are typically network connections.
/// It uses `SelectService` pattern for managing asynchronous I/O operations.
pub struct IOService<S: Selector, E, C> {
    selector: S,
    pending_endpoints: VecDeque<E>,
    io_nodes: HashMap<SelectorToken, IONode<S::Target, E>>,
    idle_strategy: IdleStrategy,
    next_endpoint_create_time_ns: u64,
    context: PhantomData<C>,
    auto_disconnect: Option<Duration>,
}

/// Defines how an instance that implements `SelectService` can be transformed
/// into an [`IOService`], facilitating the management of asynchronous I/O operations.
pub trait IntoIOService<E> {
    fn into_io_service(self, idle_strategy: IdleStrategy) -> IOService<Self, E, ()>
    where
        Self: Selector,
        Self: Sized;
}

/// Defines how an instance that implements [`Selector`] can be transformed
/// into an [`IOService`] with [`Context`], facilitating the management of asynchronous I/O operations.
pub trait IntoIOServiceWithContext<E, C: Context> {
    fn into_io_service_with_context(self, idle_strategy: IdleStrategy, context: &mut C) -> IOService<Self, E, C>
    where
        Self: Selector,
        Self: Sized;
}

impl<S: Selector, E, C> IOService<S, E, C> {
    /// Creates new instance of [`IOService`].
    pub fn new(selector: S, idle_strategy: IdleStrategy) -> IOService<S, E, C> {
        Self {
            selector,
            pending_endpoints: VecDeque::new(),
            io_nodes: HashMap::new(),
            idle_strategy,
            next_endpoint_create_time_ns: 0,
            context: PhantomData,
            auto_disconnect: None,
        }
    }

    /// Specify TTL for each [`Endpoint`] connection.
    pub fn with_auto_disconnect(self, auto_disconnect: Duration) -> IOService<S, E, C> {
        Self {
            selector: self.selector,
            pending_endpoints: self.pending_endpoints,
            io_nodes: self.io_nodes,
            idle_strategy: self.idle_strategy,
            next_endpoint_create_time_ns: self.next_endpoint_create_time_ns,
            context: self.context,
            auto_disconnect: Some(auto_disconnect),
        }
    }

    /// Registers a new [`Endpoint`] with the service.
    pub fn register(&mut self, endpoint: E) {
        self.pending_endpoints.push_back(endpoint)
    }

    fn resolve_dns(addr: &str) -> io::Result<SocketAddr> {
        addr.to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::other("unable to resolve dns address"))
    }
}

impl<S, E> IOService<S, E, ()>
where
    S: Selector,
    E: Endpoint<Target = S::Target>,
{
    /// This method polls all registered endpoints for readiness and performs I/O operations based
    /// on the ['Selector'] poll results. It then iterates through all endpoints, either
    /// updating existing streams or creating and registering new ones. It uses [`Endpoint::can_recreate`]
    /// to determine if the error that occurred during polling is recoverable (typically due to remote peer disconnect).
    pub fn poll(&mut self) -> io::Result<()> {
        // check for pending endpoints (one at a time & throttled)
        if !self.pending_endpoints.is_empty() {
            let current_time_ns = current_time_nanos();
            if current_time_ns > self.next_endpoint_create_time_ns {
                if let Some(endpoint) = self.pending_endpoints.pop_front() {
                    let addr = Self::resolve_dns(&endpoint.connection_info()?.to_string())?;
                    let stream = endpoint.create_target(addr)?;
                    let mut io_node = IONode::new(stream, endpoint, self.auto_disconnect);
                    let token = self.selector.register(&mut io_node)?;
                    self.io_nodes.insert(token, io_node);
                }
                self.next_endpoint_create_time_ns = current_time_ns + ENDPOINT_CREATION_THROTTLE_NS;
            }
        }

        // check for readiness events
        self.selector.poll(&mut self.io_nodes)?;

        // check for auto disconnect if enabled
        if self.auto_disconnect.is_some() {
            let current_time_ns = current_time_nanos();
            self.io_nodes.retain(|_token, io_node| {
                let force_disconnect = current_time_ns > io_node.disconnect_time_ns;
                if force_disconnect {
                    error!("error when polling endpoint: auto disconnected after {:?}", self.auto_disconnect.unwrap());
                    self.selector.unregister(io_node).unwrap();
                    // we need to transfer the ownership
                    // the original io_node will be dropped anyway
                    let mut endpoint =
                        unsafe { mem::replace(&mut io_node.endpoint, MaybeUninit::uninit().assume_init()) };
                    if endpoint.can_recreate() {
                        self.pending_endpoints.push_back(endpoint);
                    } else {
                        panic!("unrecoverable error when polling endpoint");
                    }
                }
                !force_disconnect
            });
        }

        // poll endpoints
        self.io_nodes.retain(|_token, io_node| {
            let (stream, endpoint) = io_node.as_parts_mut();
            if let Err(err) = endpoint.poll(stream) {
                error!("error when polling endpoint: {}", err);
                self.selector.unregister(io_node).unwrap();
                // we need to transfer the ownership
                // the original io_node will be dropped anyway
                let mut endpoint = unsafe { mem::replace(&mut io_node.endpoint, MaybeUninit::uninit().assume_init()) };
                if endpoint.can_recreate() {
                    self.pending_endpoints.push_back(endpoint);
                } else {
                    panic!("unrecoverable error when polling endpoint");
                }
                return false;
            }
            true
        });

        self.idle_strategy.idle(0);

        Ok(())
    }
}

impl<S, E, C> IOService<S, E, C>
where
    S: Selector,
    C: Context,
    E: EndpointWithContext<C, Target = S::Target>,
{
    /// This method polls all registered endpoints for readiness passing the [`Context`] and performs I/O operations based
    /// on the `SelectService` poll results. It then iterates through all endpoints, either
    /// updating existing streams or creating and registering new ones. It uses [`Endpoint::can_recreate`]
    /// to determine if the error that occurred during polling is recoverable (typically due to remote peer disconnect).
    pub fn poll(&mut self, context: &mut C) -> io::Result<()> {
        // check for pending endpoints (one at a time & throttled)
        if !self.pending_endpoints.is_empty() {
            let current_time_ns = current_time_nanos();
            if current_time_ns > self.next_endpoint_create_time_ns {
                if let Some(endpoint) = self.pending_endpoints.pop_front() {
                    let addr = Self::resolve_dns(&endpoint.connection_info()?.to_string())?;
                    let stream = endpoint.create_target(addr, context)?;
                    let mut io_node = IONode::new(stream, endpoint, self.auto_disconnect);
                    let token = self.selector.register(&mut io_node)?;
                    self.io_nodes.insert(token, io_node);
                }
                self.next_endpoint_create_time_ns = current_time_ns + ENDPOINT_CREATION_THROTTLE_NS;
            }
        }

        // check for readiness events
        self.selector.poll(&mut self.io_nodes)?;

        // check for auto disconnect if enabled
        if self.auto_disconnect.is_some() {
            let current_time_ns = current_time_nanos();
            self.io_nodes.retain(|_token, io_node| {
                let force_disconnect = current_time_ns > io_node.disconnect_time_ns;
                if force_disconnect {
                    error!("error when polling endpoint: auto disconnected after {:?}", self.auto_disconnect.unwrap());
                    self.selector.unregister(io_node).unwrap();
                    // we need to transfer the ownership
                    // the original io_node will be dropped anyway
                    let mut endpoint =
                        unsafe { mem::replace(&mut io_node.endpoint, MaybeUninit::uninit().assume_init()) };
                    if endpoint.can_recreate() {
                        self.pending_endpoints.push_back(endpoint);
                    } else {
                        panic!("unrecoverable error when polling endpoint");
                    }
                }
                !force_disconnect
            });
        }

        // poll endpoints
        self.io_nodes.retain(|_token, io_node| {
            let (stream, endpoint) = io_node.as_parts_mut();
            if let Err(err) = endpoint.poll(stream, context) {
                error!("error when polling endpoint: {}", err);
                self.selector.unregister(io_node).unwrap();
                // we need to transfer the ownership
                // the original io_node will be dropped anyway
                let mut endpoint = unsafe { mem::replace(&mut io_node.endpoint, MaybeUninit::uninit().assume_init()) };
                if endpoint.can_recreate() {
                    self.pending_endpoints.push_back(endpoint);
                } else {
                    panic!("unrecoverable error when polling endpoint");
                }
                return false;
            }
            true
        });

        Ok(())
    }
}
