//! Service to manage multiple endpoint lifecycle.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::marker::PhantomData;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use log::{error, warn};

use crate::service::endpoint::{Context, Endpoint, EndpointWithContext};
use crate::service::node::IONode;
use crate::service::select::{Selector, SelectorToken};
use crate::stream::ConnectionInfo;
use crate::util::current_time_nanos;

pub mod endpoint;
mod node;
pub mod select;

const ENDPOINT_CREATION_THROTTLE_NS: u64 = Duration::from_secs(1).as_nanos() as u64;

/// Handles the lifecycle of endpoints (see [`Endpoint`]), which are typically network connections.
/// It uses `SelectService` pattern for managing asynchronous I/O operations.
pub struct IOService<S: Selector, E, C> {
    selector: S,
    pending_endpoints: VecDeque<E>,
    io_nodes: HashMap<SelectorToken, IONode<S::Target, E>>,
    next_endpoint_create_time_ns: u64,
    context: PhantomData<C>,
    auto_disconnect: Option<Duration>,
}

/// Defines how an instance that implements `SelectService` can be transformed
/// into an [`IOService`], facilitating the management of asynchronous I/O operations.
pub trait IntoIOService<E> {
    fn into_io_service(self) -> IOService<Self, E, ()>
    where
        Self: Selector,
        Self: Sized;
}

/// Defines how an instance that implements [`Selector`] can be transformed
/// into an [`IOService`] with [`Context`], facilitating the management of asynchronous I/O operations.
pub trait IntoIOServiceWithContext<E, C: Context> {
    fn into_io_service_with_context(self, context: &mut C) -> IOService<Self, E, C>
    where
        Self: Selector,
        Self: Sized;
}

impl<S: Selector, E, C> IOService<S, E, C> {
    /// Creates new instance of [`IOService`].
    pub fn new(selector: S) -> IOService<S, E, C> {
        Self {
            selector,
            pending_endpoints: VecDeque::new(),
            io_nodes: HashMap::new(),
            next_endpoint_create_time_ns: 0,
            context: PhantomData,
            auto_disconnect: None,
        }
    }

    /// Specify TTL for each [`Endpoint`] connection.
    pub fn with_auto_disconnect(self, auto_disconnect: Duration) -> IOService<S, E, C> {
        Self {
            auto_disconnect: Some(auto_disconnect),
            ..self
        }
    }

    /// Registers a new [`Endpoint`] with the service.
    pub fn register(&mut self, endpoint: E) {
        self.pending_endpoints.push_back(endpoint)
    }

    fn resolve_dns(connection_info: &ConnectionInfo) -> io::Result<SocketAddr> {
        connection_info
            .to_socket_addrs()?
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
                if let Some(mut endpoint) = self.pending_endpoints.pop_front() {
                    let addr = Self::resolve_dns(endpoint.connection_info())?;
                    match endpoint.create_target(addr)? {
                        Some(stream) => {
                            let mut io_node = IONode::new(stream, endpoint, self.auto_disconnect);
                            let token = self.selector.register(&mut io_node)?;
                            self.io_nodes.insert(token, io_node);
                        }
                        None => self.pending_endpoints.push_back(endpoint),
                    }
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
                    // check if we really have to disconnect
                    return if io_node.as_endpoint_mut().can_auto_disconnect() {
                        warn!("endpoint auto disconnected after {:?}", self.auto_disconnect.unwrap());
                        self.selector.unregister(io_node).unwrap();
                        let mut endpoint = io_node.endpoint.take().unwrap();
                        if endpoint.can_recreate() {
                            self.pending_endpoints.push_back(endpoint);
                        } else {
                            panic!("unrecoverable error when polling endpoint");
                        }
                        false
                    } else {
                        // extend the endpoint TTL
                        io_node.disconnect_time_ns += self.auto_disconnect.unwrap().as_nanos() as u64;
                        true
                    };
                }
                true
            });
        }

        // poll endpoints
        self.io_nodes.retain(|_token, io_node| {
            let (stream, endpoint) = io_node.as_parts_mut();
            if let Err(err) = endpoint.poll(stream) {
                error!("error when polling endpoint [{}]: {}", endpoint.connection_info().host(), err);
                self.selector.unregister(io_node).unwrap();
                let mut endpoint = io_node.endpoint.take().unwrap();
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
                if let Some(mut endpoint) = self.pending_endpoints.pop_front() {
                    let addr = Self::resolve_dns(endpoint.connection_info())?;
                    match endpoint.create_target(addr, context)? {
                        Some(stream) => {
                            let mut io_node = IONode::new(stream, endpoint, self.auto_disconnect);
                            let token = self.selector.register(&mut io_node)?;
                            self.io_nodes.insert(token, io_node);
                        }
                        None => self.pending_endpoints.push_back(endpoint),
                    }
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
                    // check if we really have to disconnect
                    return if io_node.as_endpoint_mut().can_auto_disconnect(context) {
                        warn!("endpoint auto disconnected after {:?}", self.auto_disconnect.unwrap());
                        self.selector.unregister(io_node).unwrap();
                        let mut endpoint = io_node.endpoint.take().unwrap();
                        if endpoint.can_recreate(context) {
                            self.pending_endpoints.push_back(endpoint);
                        } else {
                            panic!("unrecoverable error when polling endpoint");
                        }
                        false
                    } else {
                        // extend the endpoint TTL
                        io_node.disconnect_time_ns += self.auto_disconnect.unwrap().as_nanos() as u64;
                        true
                    };
                }
                true
            });
        }

        // poll endpoints
        self.io_nodes.retain(|_token, io_node| {
            let (stream, endpoint) = io_node.as_parts_mut();
            if let Err(err) = endpoint.poll(stream, context) {
                error!("error when polling endpoint [{}]: {}", endpoint.connection_info().host(), err);
                self.selector.unregister(io_node).unwrap();
                let mut endpoint = io_node.endpoint.take().unwrap();
                if endpoint.can_recreate(context) {
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
