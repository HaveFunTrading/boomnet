//! Service to manage multiple endpoint lifecycle.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Duration;

use crate::service::dns::{BlockingDnsResolver, DnsQuery, DnsResolver};
use crate::service::endpoint::{Context, DisconnectReason, Endpoint, EndpointWithContext};
use crate::service::node::IONode;
use crate::service::select::{Selector, SelectorToken};
use crate::service::time::{SystemTimeClockSource, TimeSource};
use crate::stream::ConnectionInfoProvider;

pub mod dns;
pub mod endpoint;
mod node;
pub mod select;
pub mod time;

const ENDPOINT_CREATION_THROTTLE_NS: u64 = Duration::from_secs(1).as_nanos() as u64;

const DNS_RESOLVE_TIMEOUT_NS: u64 = Duration::from_secs(5).as_nanos() as u64;

/// Endpoint handle.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
#[repr(transparent)]
pub struct Handle(SelectorToken);

/// Handles the lifecycle of endpoints (see [`Endpoint`]), which are typically network connections.
/// It uses `SelectService` pattern for managing asynchronous I/O operations.
pub struct IOService<S: Selector, E, C, TS, D: DnsResolver> {
    selector: S,
    pending_endpoints: VecDeque<(Handle, D::Query, u64, E)>,
    io_nodes: HashMap<SelectorToken, IONode<S::Target, E>>,
    next_endpoint_create_time_ns: u64,
    context: PhantomData<C>,
    auto_disconnect: Option<Box<dyn Fn() -> Duration>>,
    time_source: TS,
    dns_resolver: D,
}

/// Defines how an instance that implements `SelectService` can be transformed
/// into an [`IOService`], facilitating the management of asynchronous I/O operations.
pub trait IntoIOService<E> {
    fn into_io_service(self) -> IOService<Self, E, (), SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized;
}

/// Defines how an instance that implements [`Selector`] can be transformed
/// into an [`IOService`] with [`Context`], facilitating the management of asynchronous I/O operations.
pub trait IntoIOServiceWithContext<E, C: Context> {
    fn into_io_service_with_context(
        self,
        context: &mut C,
    ) -> IOService<Self, E, C, SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized;
}

impl<S: Selector, E, C, TS, D: DnsResolver> IOService<S, E, C, TS, D> {
    /// Creates new instance of [`IOService`].
    pub fn new(selector: S, time_source: TS, dns_resolver: D) -> IOService<S, E, C, TS, D> {
        Self {
            selector,
            pending_endpoints: VecDeque::new(),
            io_nodes: HashMap::new(),
            next_endpoint_create_time_ns: 0,
            context: PhantomData,
            auto_disconnect: None,
            time_source,
            dns_resolver,
        }
    }

    /// Specify TTL for each [`Endpoint`] connection.
    pub fn with_auto_disconnect(self, auto_disconnect: Duration) -> IOService<S, E, C, TS, D> {
        self.with_auto_disconnect_supplier(move || auto_disconnect)
    }

    /// Specify TTL supplier for each [`Endpoint`] connection.
    pub fn with_auto_disconnect_supplier<F>(self, f: F) -> IOService<S, E, C, TS, D>
    where
        F: Fn() -> Duration + 'static,
    {
        Self {
            auto_disconnect: Some(Box::new(f)),
            ..self
        }
    }

    /// Specify custom [`TimeSource`] instead of the default system time source.
    pub fn with_time_source<T: TimeSource>(self, time_source: T) -> IOService<S, E, C, T, D> {
        IOService {
            time_source,
            pending_endpoints: Default::default(),
            context: self.context,
            auto_disconnect: self.auto_disconnect,
            io_nodes: Default::default(),
            next_endpoint_create_time_ns: self.next_endpoint_create_time_ns,
            selector: self.selector,
            dns_resolver: self.dns_resolver,
        }
    }

    /// Specify custom [`TimeSource`] instead of the default system time source.
    pub fn with_dns_resolver<DR: DnsResolver>(self, dns_resolver: DR) -> IOService<S, E, C, TS, DR> {
        IOService {
            time_source: self.time_source,
            pending_endpoints: Default::default(),
            context: self.context,
            auto_disconnect: self.auto_disconnect,
            io_nodes: Default::default(),
            next_endpoint_create_time_ns: self.next_endpoint_create_time_ns,
            selector: self.selector,
            dns_resolver,
        }
    }

    /// Registers a new [`Endpoint`] with the service and return a handle to it.
    pub fn register(&mut self, endpoint: E) -> io::Result<Handle>
    where
        E: ConnectionInfoProvider,
        TS: TimeSource,
    {
        let handle = Handle(self.selector.next_token());
        let info = endpoint.connection_info();
        let query = self.dns_resolver.new_query(info.host(), info.port())?;
        let now = self.time_source.current_time_nanos();
        self.pending_endpoints.push_back((handle, query, now, endpoint));
        Ok(handle)
    }

    /// Deregister [`Endpoint`] with the service based on a handle.
    pub fn deregister(&mut self, handle: Handle) -> Option<E> {
        match self.io_nodes.remove(&handle.0) {
            Some(io_node) => Some(io_node.into_endpoint().1),
            None => {
                let mut index_to_remove = None;
                for (index, endpoint) in self.pending_endpoints.iter().enumerate() {
                    if endpoint.0 == handle {
                        index_to_remove = Some(index);
                        break;
                    }
                }
                if let Some(index_to_remove) = index_to_remove {
                    self.pending_endpoints
                        .remove(index_to_remove)
                        .map(|(_, _, _, endpoint)| endpoint)
                } else {
                    None
                }
            }
        }
    }

    /// Return iterator over active endpoints, additionally exposing handle and the stream.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &S::Target, &E)> {
        self.io_nodes.values().map(|io_node| {
            let (stream, (handle, endpoint)) = io_node.as_parts();
            (*handle, stream, endpoint)
        })
    }

    /// Return mutable iterator over active endpoints, additionally exposing handle and the stream.
    #[inline]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Handle, &mut S::Target, &mut E)> {
        self.io_nodes.values_mut().map(|io_node| {
            let (stream, (handle, endpoint)) = io_node.as_parts_mut();
            (*handle, stream, endpoint)
        })
    }

    /// Return iterator over pending endpoints.
    #[inline]
    pub fn pending(&self) -> impl Iterator<Item = (&Handle, &E)> {
        self.pending_endpoints
            .iter()
            .map(|(handle, _, _, endpoint)| (handle, endpoint))
    }

    #[inline]
    fn resolve_dns(&self, query: &mut impl DnsQuery, created_time_ns: u64) -> io::Result<Option<SocketAddr>>
    where
        TS: TimeSource,
    {
        let now = self.time_source.current_time_nanos();
        if now > created_time_ns + DNS_RESOLVE_TIMEOUT_NS {
            return Err(io::Error::new(ErrorKind::TimedOut, "dns resolution timed out"));
        }
        match query.poll() {
            Ok(addrs) => {
                let addr = addrs
                    .into_iter()
                    .next()
                    .ok_or_else(|| io::Error::other("dns resolution dio not return any address"))?;
                Ok(Some(addr))
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(err) => Err(err),
        }
    }
}

impl<S, E, TS, D> IOService<S, E, (), TS, D>
where
    S: Selector,
    E: Endpoint<Target = S::Target>,
    TS: TimeSource,
    D: DnsResolver,
{
    /// This method polls all registered endpoints for readiness and performs I/O operations based
    /// on the ['Selector'] poll results. It then iterates through all endpoints, either
    /// updating existing streams or creating and registering new ones. It uses [`Endpoint::can_recreate`]
    /// to determine if the error that occurred during polling is recoverable (typically due to remote peer disconnect).
    pub fn poll(&mut self) -> io::Result<()> {
        // check for pending endpoints (one at a time & throttled)
        if !self.pending_endpoints.is_empty() {
            let current_time_ns = self.time_source.current_time_nanos();
            if current_time_ns > self.next_endpoint_create_time_ns {
                if let Some((handle, mut query, query_time_ns, mut endpoint)) = self.pending_endpoints.pop_front() {
                    if let Some(addr) = self.resolve_dns(&mut query, query_time_ns)? {
                        match endpoint.create_target(addr)? {
                            Some(stream) => {
                                let ttl = self.auto_disconnect.as_ref().map(|auto_disconnect| auto_disconnect());
                                let mut io_node = IONode::new(stream, handle, endpoint, ttl, &self.time_source, addr);
                                self.selector.register(handle.0, &mut io_node)?;
                                self.io_nodes.insert(handle.0, io_node);
                            }
                            None => {
                                // request new dns query
                                let info = endpoint.connection_info();
                                let query = self.dns_resolver.new_query(info.host(), info.port())?;
                                let now = self.time_source.current_time_nanos();
                                self.pending_endpoints.push_back((handle, query, now, endpoint))
                            }
                        }
                    } else {
                        self.pending_endpoints
                            .push_back((handle, query, query_time_ns, endpoint))
                    }
                }
                self.next_endpoint_create_time_ns = current_time_ns + ENDPOINT_CREATION_THROTTLE_NS;
            }
        }

        // check for readiness events
        self.selector.poll(&mut self.io_nodes)?;

        // check for auto disconnect if enabled
        if let Some(auto_disconnect) = self.auto_disconnect.as_ref() {
            let current_time_ns = self.time_source.current_time_nanos();
            self.io_nodes.retain(|_token, io_node| {
                let force_disconnect = current_time_ns > io_node.disconnect_time_ns;
                if force_disconnect {
                    // check if we really have to disconnect
                    return if io_node.as_endpoint_mut().1.can_auto_disconnect() {
                        self.selector.unregister(io_node).unwrap();
                        let (handle, mut endpoint) = io_node.endpoint.take().unwrap();
                        if endpoint.can_recreate(DisconnectReason::auto_disconnect(io_node.ttl)) {
                            let info = endpoint.connection_info();
                            let query = self.dns_resolver.new_query(info.host(), info.port()).unwrap();
                            let now = self.time_source.current_time_nanos();
                            self.pending_endpoints.push_back((handle, query, now, endpoint));
                        } else {
                            panic!("unrecoverable error when polling endpoint");
                        }
                        false
                    } else {
                        // extend the endpoint TTL
                        let extend = auto_disconnect().as_nanos() as u64;
                        io_node.disconnect_time_ns = io_node.disconnect_time_ns.saturating_add(extend);
                        true
                    };
                }
                true
            });
        }

        // poll endpoints
        self.io_nodes.retain(|_token, io_node| {
            let (stream, (_, endpoint)) = io_node.as_parts_mut();
            if let Err(err) = endpoint.poll(stream) {
                self.selector.unregister(io_node).unwrap();
                let (handle, mut endpoint) = io_node.endpoint.take().unwrap();
                if endpoint.can_recreate(DisconnectReason::other(err)) {
                    let info = endpoint.connection_info();
                    let query = self.dns_resolver.new_query(info.host(), info.port()).unwrap();
                    let now = self.time_source.current_time_nanos();
                    self.pending_endpoints.push_back((handle, query, now, endpoint));
                } else {
                    panic!("unrecoverable error when polling endpoint");
                }
                return false;
            }
            true
        });

        Ok(())
    }

    /// Dispatch command to an active endpoint using `handle` and provided `action`. If the
    /// endpoint is currently active `true` will be returned and the provided `action` invoked,
    /// otherwise this method will return `false` and no `action` will be invoked.
    pub fn dispatch<F>(&mut self, handle: Handle, mut action: F) -> io::Result<bool>
    where
        F: FnMut(&mut E::Target, &mut E) -> std::io::Result<()>,
    {
        match self.io_nodes.get_mut(&handle.0) {
            Some(io_node) => {
                let (stream, (_, endpoint)) = io_node.as_parts_mut();
                action(stream, endpoint)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

impl<S, E, C, TS, D> IOService<S, E, C, TS, D>
where
    S: Selector,
    C: Context,
    E: EndpointWithContext<C, Target = S::Target>,
    TS: TimeSource,
    D: DnsResolver,
{
    /// This method polls all registered endpoints for readiness passing the [`Context`] and performs I/O operations based
    /// on the `SelectService` poll results. It then iterates through all endpoints, either
    /// updating existing streams or creating and registering new ones. It uses [`Endpoint::can_recreate`]
    /// to determine if the error that occurred during polling is recoverable (typically due to remote peer disconnect).
    pub fn poll(&mut self, context: &mut C) -> io::Result<()> {
        // check for pending endpoints (one at a time & throttled)
        if !self.pending_endpoints.is_empty() {
            let current_time_ns = self.time_source.current_time_nanos();
            if current_time_ns > self.next_endpoint_create_time_ns {
                if let Some((handle, mut query, query_time_ns, mut endpoint)) = self.pending_endpoints.pop_front() {
                    if let Some(addr) = self.resolve_dns(&mut query, query_time_ns)? {
                        match endpoint.create_target(addr, context)? {
                            Some(stream) => {
                                let ttl = self.auto_disconnect.as_ref().map(|auto_disconnect| auto_disconnect());
                                let mut io_node = IONode::new(stream, handle, endpoint, ttl, &self.time_source, addr);
                                self.selector.register(handle.0, &mut io_node)?;
                                self.io_nodes.insert(handle.0, io_node);
                            }
                            None => {
                                // request new dns query
                                let info = endpoint.connection_info();
                                let query = self.dns_resolver.new_query(info.host(), info.port())?;
                                let now = self.time_source.current_time_nanos();
                                self.pending_endpoints.push_back((handle, query, now, endpoint))
                            }
                        }
                    } else {
                        self.pending_endpoints
                            .push_back((handle, query, query_time_ns, endpoint))
                    }
                }
                self.next_endpoint_create_time_ns = current_time_ns + ENDPOINT_CREATION_THROTTLE_NS;
            }
        }

        // check for readiness events
        self.selector.poll(&mut self.io_nodes)?;

        // check for auto disconnect if enabled
        if let Some(auto_disconnect) = self.auto_disconnect.as_ref() {
            let current_time_ns = self.time_source.current_time_nanos();
            self.io_nodes.retain(|_token, io_node| {
                let force_disconnect = current_time_ns > io_node.disconnect_time_ns;
                if force_disconnect {
                    // check if we really have to disconnect
                    return if io_node.as_endpoint_mut().1.can_auto_disconnect(context) {
                        self.selector.unregister(io_node).unwrap();
                        let (handle, mut endpoint) = io_node.endpoint.take().unwrap();
                        if endpoint.can_recreate(DisconnectReason::auto_disconnect(io_node.ttl), context) {
                            let info = endpoint.connection_info();
                            let query = self.dns_resolver.new_query(info.host(), info.port()).unwrap();
                            let now = self.time_source.current_time_nanos();
                            self.pending_endpoints.push_back((handle, query, now, endpoint));
                        } else {
                            panic!("unrecoverable error when polling endpoint");
                        }
                        false
                    } else {
                        // extend the endpoint TTL
                        let extend = auto_disconnect().as_nanos() as u64;
                        io_node.disconnect_time_ns = io_node.disconnect_time_ns.saturating_add(extend);
                        true
                    };
                }
                true
            });
        }

        // poll endpoints
        self.io_nodes.retain(|_token, io_node| {
            let (stream, (_, endpoint)) = io_node.as_parts_mut();
            if let Err(err) = endpoint.poll(stream, context) {
                self.selector.unregister(io_node).unwrap();
                let (handle, mut endpoint) = io_node.endpoint.take().unwrap();
                if endpoint.can_recreate(DisconnectReason::other(err), context) {
                    let info = endpoint.connection_info();
                    let query = self.dns_resolver.new_query(info.host(), info.port()).unwrap();
                    let now = self.time_source.current_time_nanos();
                    self.pending_endpoints.push_back((handle, query, now, endpoint));
                } else {
                    panic!("unrecoverable error when polling endpoint");
                }
                return false;
            }
            true
        });

        Ok(())
    }

    /// Dispatch command to an active endpoint using `handle` and provided `action`. If the
    /// endpoint is currently active `true` will be returned and the provided `action` invoked,
    /// otherwise this method will return `false` and no `action` will be invoked. This method
    /// requires `Context` to be passed and exposes it to the provided `action`.
    pub fn dispatch<F>(&mut self, handle: Handle, ctx: &mut C, mut action: F) -> io::Result<bool>
    where
        F: FnMut(&mut E::Target, &mut E, &mut C) -> std::io::Result<()>,
    {
        match self.io_nodes.get_mut(&handle.0) {
            Some(io_node) => {
                let (stream, (_, endpoint)) = io_node.as_parts_mut();
                action(stream, endpoint, ctx)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}
