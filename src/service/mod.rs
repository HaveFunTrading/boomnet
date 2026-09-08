//! Manage endpoint factories and the lifecycle of their I/O endpoints.

use std::collections::VecDeque;
use std::io;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;

use crate::service::dns::{BlockingDnsResolver, DnsQuery, DnsResolver};
use crate::service::endpoint::{DisconnectReason, EndpointFactory};
use crate::service::error::IOServiceOperation;
use crate::service::node::{IONode, IONodes};
use crate::service::select::{Selector, SelectorToken};
use crate::service::time::{SystemTimeClockSource, TimeSource};
use crate::stream::ConnectionInfoProvider;

pub mod dns;
pub mod endpoint;
pub mod error;
mod node;
pub mod select;
pub mod time;

pub use error::IOServiceError;

const ENDPOINT_CREATION_THROTTLE_NS: u64 = Duration::from_secs(1).as_nanos() as u64;

/// Identifies a factory registration and its successive endpoints across reconnects.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
#[repr(transparent)]
pub struct Handle(SelectorToken);

/// Retains registered [`EndpointFactory`] values and manages the endpoints they create.
/// A [`Selector`] drives I/O readiness for the active endpoints.
pub struct IOService<S: Selector, F, TS, D: DnsResolver> {
    selector: S,
    pending_factories: VecDeque<(Handle, D::Query, u64, F)>,
    io_nodes: IONodes<S::Target, F>,
    next_endpoint_create_time_ns: u64,
    auto_disconnect: Option<Box<dyn Fn() -> Duration>>,
    time_source: TS,
    dns_resolver: D,
    dns_query_timeout_ns: Option<u64>,
}

/// One unit of endpoint lifecycle work produced by [`IOService::poll`].
#[derive(Debug)]
pub enum IOServiceEvent<'a, E> {
    /// An endpoint became active.
    Connected {
        /// Connected endpoint handle.
        handle: Handle,
    },
    /// An endpoint disconnected and was scheduled for recreation.
    Disconnected {
        /// Disconnected endpoint handle.
        handle: Handle,
        /// Cause of the disconnection.
        reason: DisconnectReason,
    },
    /// An active endpoint ready for application-defined I/O.
    Active(ActiveEndpoint<'a, E>),
}

/// Guard granting access to one active endpoint.
///
/// The endpoint is intentionally only exposed through [`ActiveEndpoint::try_with`]. Any I/O error
/// returned by the action is remembered by the service and starts the endpoint's disconnect and
/// recreation lifecycle on the next call to [`IOService::poll`].
#[derive(Debug)]
pub struct ActiveEndpoint<'a, E> {
    handle: Handle,
    endpoint: &'a mut E,
    pending_disconnect: &'a mut Option<DisconnectReason>,
}

impl<'a, E> ActiveEndpoint<'a, E> {
    /// Return the handle of the active endpoint.
    #[inline]
    pub const fn handle(&self) -> Handle {
        self.handle
    }

    /// Perform application-defined I/O with the active endpoint.
    ///
    /// The returned value may borrow the endpoint for the lifetime of this guard. If `action`
    /// returns an error, the error is returned unchanged and a copy is retained as the endpoint's
    /// disconnect reason. Iterator values remain guarded through [`ActiveOutput`], which also
    /// records errors yielded by iterators of `io::Result` items.
    #[inline]
    pub fn try_with<R>(self, action: impl FnOnce(&'a mut E) -> io::Result<R>) -> io::Result<ActiveOutput<'a, R>> {
        match action(self.endpoint) {
            Ok(value) => Ok(ActiveOutput {
                value,
                pending_disconnect: self.pending_disconnect,
            }),
            Err(source) => {
                *self.pending_disconnect = Some(DisconnectReason::IO(copy_io_error(&source)));
                Err(source)
            }
        }
    }
}

/// A value produced through [`ActiveEndpoint::try_with`] that remains connected to the endpoint's
/// lifecycle state.
///
/// When the value is an iterator yielding `io::Result<T>`, this type forwards its items and records
/// the first yielded error as a disconnect reason.
#[derive(Debug)]
pub struct ActiveOutput<'a, R> {
    value: R,
    pending_disconnect: &'a mut Option<DisconnectReason>,
}

impl<R> ActiveOutput<'_, R> {
    /// Consume the guard and return its inner value.
    ///
    /// Use this for non-iterator outputs. Extracting an iterator opts out of automatic tracking of
    /// errors yielded after [`ActiveEndpoint::try_with`] returns.
    #[inline]
    pub fn into_inner(self) -> R {
        self.value
    }
}

impl<I, T> Iterator for ActiveOutput<'_, I>
where
    I: Iterator<Item = io::Result<T>>,
{
    type Item = io::Result<T>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.value.next()?;
        if let Err(source) = &item
            && self.pending_disconnect.is_none()
        {
            *self.pending_disconnect = Some(DisconnectReason::IO(copy_io_error(source)));
        }
        Some(item)
    }
}

fn copy_io_error(source: &io::Error) -> io::Error {
    match source.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code),
        None => io::Error::new(source.kind(), source.to_string()),
    }
}

#[derive(Debug)]
enum LifecycleEvent {
    Connected { handle: Handle },
    Disconnected { handle: Handle, reason: DisconnectReason },
}

enum IOServiceEventsInner<'a, F: EndpointFactory> {
    Lifecycle(Option<LifecycleEvent>),
    Active(std::slice::IterMut<'a, Option<IONode<F::Endpoint, F>>>),
}

/// Iterator over the result of one service poll.
///
/// A poll that performs a lifecycle transition yields exactly one lifecycle event. Otherwise,
/// the iterator visits every active endpoint once.
pub struct IOServiceEvents<'a, F: EndpointFactory> {
    inner: IOServiceEventsInner<'a, F>,
}

impl<'a, F: EndpointFactory> IOServiceEvents<'a, F> {
    #[inline]
    fn lifecycle(event: LifecycleEvent) -> Self {
        Self {
            inner: IOServiceEventsInner::Lifecycle(Some(event)),
        }
    }

    #[inline]
    fn active(nodes: std::slice::IterMut<'a, Option<IONode<F::Endpoint, F>>>) -> Self {
        Self {
            inner: IOServiceEventsInner::Active(nodes),
        }
    }
}

impl<'a, F: EndpointFactory> Iterator for IOServiceEvents<'a, F> {
    type Item = IOServiceEvent<'a, F::Endpoint>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IOServiceEventsInner::Lifecycle(event) => event.take().map(|event| match event {
                LifecycleEvent::Connected { handle } => IOServiceEvent::Connected { handle },
                LifecycleEvent::Disconnected { handle, reason } => IOServiceEvent::Disconnected { handle, reason },
            }),
            IOServiceEventsInner::Active(nodes) => {
                for node in nodes.by_ref().flatten() {
                    if node.pending_disconnect.is_some() {
                        continue;
                    }
                    return Some(IOServiceEvent::Active(ActiveEndpoint {
                        handle: node.factory.0,
                        endpoint: &mut node.endpoint,
                        pending_disconnect: &mut node.pending_disconnect,
                    }));
                }
                None
            }
        }
    }
}

/// Defines how an instance that implements [`Selector`] can be transformed
/// into an [`IOService`], using the factory's associated endpoint and context types.
pub trait IntoIOService<F> {
    fn into_io_service(self) -> IOService<Self, F, SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized;
}

impl<S: Selector, F, TS, D: DnsResolver> IOService<S, F, TS, D> {
    /// Creates new instance of [`IOService`].
    pub fn new(selector: S, time_source: TS, dns_resolver: D) -> IOService<S, F, TS, D> {
        Self {
            selector,
            pending_factories: VecDeque::new(),
            io_nodes: IONodes::default(),
            next_endpoint_create_time_ns: 0,
            auto_disconnect: None,
            time_source,
            dns_resolver,
            dns_query_timeout_ns: None,
        }
    }

    /// Specify TTL for each created endpoint.
    pub fn with_auto_disconnect(self, auto_disconnect: Duration) -> IOService<S, F, TS, D> {
        self.with_auto_disconnect_supplier(move || auto_disconnect)
    }

    /// Specify a TTL supplier for each created endpoint.
    pub fn with_auto_disconnect_supplier<A>(self, f: A) -> IOService<S, F, TS, D>
    where
        A: Fn() -> Duration + 'static,
    {
        Self {
            auto_disconnect: Some(Box::new(f)),
            ..self
        }
    }

    /// Specify DNS query timeout. This is only relevant when using asynchronous form of
    /// [`DnsResolver`].
    pub fn with_dns_query_timeout(self, timeout: Duration) -> IOService<S, F, TS, D> {
        Self {
            dns_query_timeout_ns: Some(timeout.as_nanos() as u64),
            ..self
        }
    }

    /// Specify custom [`TimeSource`] instead of the default system time source.
    pub fn with_time_source<T: TimeSource>(self, time_source: T) -> IOService<S, F, T, D> {
        IOService {
            time_source,
            pending_factories: Default::default(),
            auto_disconnect: self.auto_disconnect,
            io_nodes: Default::default(),
            next_endpoint_create_time_ns: self.next_endpoint_create_time_ns,
            selector: self.selector,
            dns_resolver: self.dns_resolver,
            dns_query_timeout_ns: self.dns_query_timeout_ns,
        }
    }

    /// Specify custom [`TimeSource`] instead of the default system time source.
    pub fn with_dns_resolver<DR: DnsResolver>(self, dns_resolver: DR) -> IOService<S, F, TS, DR> {
        IOService {
            time_source: self.time_source,
            pending_factories: Default::default(),
            auto_disconnect: self.auto_disconnect,
            io_nodes: Default::default(),
            next_endpoint_create_time_ns: self.next_endpoint_create_time_ns,
            selector: self.selector,
            dns_resolver,
            dns_query_timeout_ns: self.dns_query_timeout_ns,
        }
    }

    /// Register an [`EndpointFactory`] and return its handle.
    ///
    /// The service creates the endpoint during [`Self::poll`]. The handle remains the same
    /// when the factory creates replacement endpoints after disconnects.
    pub fn register(&mut self, factory: F) -> Result<Handle, IOServiceError>
    where
        F: ConnectionInfoProvider,
        TS: TimeSource,
    {
        let handle = Handle(self.selector.next_token());
        let info = factory.connection_info();
        let query = self
            .dns_resolver
            .new_query(info.host(), info.port())
            .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::Resolve, source))?;
        let now = self.time_source.current_time_nanos();
        self.pending_factories.push_back((handle, query, now, factory));
        Ok(handle)
    }

    /// Build and register an [`EndpointFactory`], passing its handle to `build_factory`.
    ///
    /// Use this when the factory needs to know its registration handle. Endpoint creation
    /// happens later during [`Self::poll`].
    pub fn register_with<A>(&mut self, build_factory: A) -> Result<Handle, IOServiceError>
    where
        F: ConnectionInfoProvider,
        TS: TimeSource,
        A: FnOnce(Handle) -> io::Result<F>,
    {
        let handle = Handle(self.selector.next_token());
        let factory = build_factory(handle)
            .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::CreateFactory, source))?;
        let info = factory.connection_info();
        let query = self
            .dns_resolver
            .new_query(info.host(), info.port())
            .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::Resolve, source))?;
        let now = self.time_source.current_time_nanos();
        self.pending_factories.push_back((handle, query, now, factory));
        Ok(handle)
    }

    /// Remove a registration and its active endpoint, if any, and return the factory.
    pub fn deregister(&mut self, handle: Handle) -> Result<Option<F>, IOServiceError> {
        if let Some(io_node) = self.io_nodes.get_mut(handle.0) {
            self.selector
                .unregister(io_node)
                .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::Unregister, source))?;
            match self.io_nodes.remove(handle.0) {
                Some(io_node) => Ok(Some(io_node.into_factory().1)),
                None => Err(IOServiceError::InvalidState {
                    handle: Some(handle),
                    message: "endpoint disappeared after selector unregistration",
                }),
            }
        } else {
            let mut index_to_remove = None;
            for (index, factory) in self.pending_factories.iter().enumerate() {
                if factory.0 == handle {
                    index_to_remove = Some(index);
                    break;
                }
            }
            if let Some(index_to_remove) = index_to_remove {
                Ok(self
                    .pending_factories
                    .remove(index_to_remove)
                    .map(|(_, _, _, factory)| factory))
            } else {
                Ok(None)
            }
        }
    }

    /// Iterate over active registrations as `(handle, endpoint, factory)`.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &S::Target, &F)> {
        self.io_nodes.values().map(|io_node| {
            let (endpoint, (handle, factory)) = io_node.as_parts();
            (*handle, endpoint, factory)
        })
    }

    /// Iterate mutably over active registrations as `(handle, endpoint, factory)`.
    #[inline]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Handle, &mut S::Target, &mut F)> {
        self.io_nodes.values_mut().map(|io_node| {
            let (endpoint, (handle, factory)) = io_node.as_parts_mut();
            (*handle, endpoint, factory)
        })
    }

    /// Iterate over `(handle, factory)` pairs awaiting endpoint creation.
    #[inline]
    pub fn pending(&self) -> impl Iterator<Item = (&Handle, &F)> {
        self.pending_factories
            .iter()
            .map(|(handle, _, _, factory)| (handle, factory))
    }

    #[inline]
    fn resolve_dns(&self, query: &mut impl DnsQuery, created_time_ns: u64) -> io::Result<Option<SocketAddr>>
    where
        TS: TimeSource,
    {
        // check if dns query resolution timed out
        if let Some(dns_query_timeout) = self.dns_query_timeout_ns {
            let now = self.time_source.current_time_nanos();
            if now > created_time_ns + dns_query_timeout {
                return Err(io::Error::new(ErrorKind::TimedOut, "dns resolution timed out"));
            }
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

    #[cold]
    fn check_pending_factories<A>(&mut self, create_endpoint: A) -> Result<Option<Handle>, IOServiceError>
    where
        F: ConnectionInfoProvider,
        TS: TimeSource,
        A: FnOnce(&mut F, SocketAddr) -> io::Result<Option<<S as Selector>::Target>>,
    {
        let current_time_ns = self.time_source.current_time_nanos();
        if current_time_ns > self.next_endpoint_create_time_ns {
            if let Some((handle, mut query, query_time_ns, mut factory)) = self.pending_factories.pop_front() {
                if let Some(addr) = self
                    .resolve_dns(&mut query, query_time_ns)
                    .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::Resolve, source))?
                {
                    match create_endpoint(&mut factory, addr).map_err(|source| {
                        IOServiceError::io(Some(handle), IOServiceOperation::CreateEndpoint, source)
                    })? {
                        Some(endpoint) => {
                            let ttl = self.auto_disconnect.as_ref().map(|auto_disconnect| auto_disconnect());
                            let mut io_node = IONode::new(endpoint, handle, factory, ttl, &self.time_source);
                            self.selector.register(handle.0, &mut io_node).map_err(|source| {
                                IOServiceError::io(Some(handle), IOServiceOperation::Register, source)
                            })?;
                            self.io_nodes
                                .insert(handle.0, io_node)
                                .map_err(|_| IOServiceError::InvalidState {
                                    handle: Some(handle),
                                    message: "endpoint token is already active",
                                })?;
                            self.next_endpoint_create_time_ns = current_time_ns + ENDPOINT_CREATION_THROTTLE_NS;
                            return Ok(Some(handle));
                        }
                        None => {
                            // request new dns query
                            let info = factory.connection_info();
                            let query = self
                                .dns_resolver
                                .new_query(info.host(), info.port())
                                .map_err(|source| {
                                    IOServiceError::io(Some(handle), IOServiceOperation::Resolve, source)
                                })?;
                            let now = self.time_source.current_time_nanos();
                            self.pending_factories.push_back((handle, query, now, factory))
                        }
                    }
                } else {
                    self.pending_factories
                        .push_back((handle, query, query_time_ns, factory))
                }
            }
            self.next_endpoint_create_time_ns = current_time_ns + ENDPOINT_CREATION_THROTTLE_NS;
        }
        Ok(None)
    }

    fn remove_active_endpoint(&mut self, handle: Handle) -> Result<F, IOServiceError> {
        let io_node = self.io_nodes.get_mut(handle.0).ok_or(IOServiceError::InvalidState {
            handle: Some(handle),
            message: "active endpoint is not registered",
        })?;
        self.selector
            .unregister(io_node)
            .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::Unregister, source))?;
        let io_node = self.io_nodes.remove(handle.0).ok_or(IOServiceError::InvalidState {
            handle: Some(handle),
            message: "endpoint disappeared after selector unregistration",
        })?;
        Ok(io_node.into_factory().1)
    }

    #[inline]
    fn take_pending_disconnect(&mut self) -> Option<(Handle, DisconnectReason)> {
        self.io_nodes
            .values_mut()
            .find_map(|node| node.pending_disconnect.take().map(|reason| (node.factory.0, reason)))
    }

    #[inline]
    fn expired_endpoint(&self) -> Option<(Handle, Duration)>
    where
        TS: TimeSource,
    {
        self.auto_disconnect.as_ref().and_then(|_| {
            let current_time_ns = self.time_source.current_time_nanos();
            self.io_nodes
                .values()
                .find(|node| current_time_ns > node.disconnect_time_ns)
                .map(|node| (node.factory.0, node.ttl))
        })
    }

    fn defer_auto_disconnect(&mut self, handle: Handle) -> Result<(), IOServiceError> {
        let Some(extension) = self
            .auto_disconnect
            .as_ref()
            .map(|auto_disconnect| auto_disconnect().as_nanos() as u64)
        else {
            return Ok(());
        };
        let node = self.io_nodes.get_mut(handle.0).ok_or(IOServiceError::InvalidState {
            handle: Some(handle),
            message: "expired endpoint is not registered",
        })?;
        node.disconnect_time_ns = node.disconnect_time_ns.saturating_add(extension);
        Ok(())
    }

    fn disconnect_active<A>(
        &mut self,
        handle: Handle,
        reason: DisconnectReason,
        can_recreate: A,
    ) -> Result<LifecycleEvent, IOServiceError>
    where
        F: ConnectionInfoProvider,
        TS: TimeSource,
        A: FnOnce(&mut F, &DisconnectReason) -> bool,
    {
        let recreate = {
            let node = self.io_nodes.get_mut(handle.0).ok_or(IOServiceError::InvalidState {
                handle: Some(handle),
                message: "disconnected endpoint is not registered",
            })?;
            can_recreate(&mut node.as_factory_mut().1, &reason)
        };
        let factory = self.remove_active_endpoint(handle)?;
        if !recreate {
            return Err(IOServiceError::EndpointNotRecreatable { handle, reason });
        }
        self.schedule_reconnect(handle, factory)?;
        Ok(LifecycleEvent::Disconnected { handle, reason })
    }

    fn schedule_reconnect(&mut self, handle: Handle, factory: F) -> Result<(), IOServiceError>
    where
        F: ConnectionInfoProvider,
        TS: TimeSource,
    {
        let info = factory.connection_info();
        let query = self
            .dns_resolver
            .new_query(info.host(), info.port())
            .map_err(|source| IOServiceError::io(Some(handle), IOServiceOperation::Resolve, source))?;
        let now = self.time_source.current_time_nanos();
        self.pending_factories.push_back((handle, query, now, factory));
        Ok(())
    }
}

impl<S, F, TS, D> IOService<S, F, TS, D>
where
    S: Selector,
    F: EndpointFactory<Endpoint = S::Target>,
    TS: TimeSource,
    D: DnsResolver,
{
    /// Poll the selector once and return an iterator over endpoint lifecycle events.
    ///
    /// Pass `&mut ()` for factories whose [`EndpointFactory::Context`] is `()`.
    /// Each endpoint contributes at most one event; an empty iterator means no endpoint produced work.
    ///
    /// The returned iterator borrows the service, but does not borrow `ctx`, allowing application
    /// logic to use its context while processing active endpoints.
    pub fn poll(&mut self, ctx: &mut F::Context) -> Result<IOServiceEvents<'_, F>, IOServiceError> {
        let lifecycle = if let Some((handle, reason)) = self.take_pending_disconnect() {
            Some(self.disconnect_active(handle, reason, |factory, reason| factory.can_recreate(reason, ctx))?)
        } else if let Some((handle, ttl)) = self.expired_endpoint() {
            if self
                .io_nodes
                .get_mut(handle.0)
                .ok_or(IOServiceError::InvalidState {
                    handle: Some(handle),
                    message: "expired endpoint is not registered",
                })?
                .as_factory_mut()
                .1
                .can_auto_disconnect(ctx)
            {
                let reason = DisconnectReason::AutoDisconnect(ttl);
                Some(self.disconnect_active(handle, reason, |factory, reason| factory.can_recreate(reason, ctx))?)
            } else {
                self.defer_auto_disconnect(handle)?;
                None
            }
        } else if self.pending_factories.is_empty() {
            None
        } else {
            self.check_pending_factories(|factory, addr| factory.create_endpoint(addr, ctx))?
                .map(|handle| LifecycleEvent::Connected { handle })
        };

        if let Some(lifecycle) = lifecycle {
            return Ok(IOServiceEvents::lifecycle(lifecycle));
        }

        self.selector
            .poll(&mut self.io_nodes)
            .map_err(|source| IOServiceError::io(None, IOServiceOperation::PollSelector, source))?;

        Ok(IOServiceEvents::active(self.io_nodes.slots_mut()))
    }

    /// Dispatch command to an active endpoint using `handle` and provided `action`. If the
    /// endpoint is currently active `Ok(Some(...))` will be returned and the provided `action` invoked,
    /// otherwise this method will return `Ok(None)` and no `action` will be invoked.
    /// The action receives the live endpoint and its factory, and can capture application context directly.
    pub fn dispatch<A, T>(&mut self, handle: Handle, mut action: A) -> io::Result<Option<T>>
    where
        A: FnMut(&mut F::Endpoint, &mut F) -> std::io::Result<T>,
    {
        match self.io_nodes.get_mut(handle.0) {
            Some(io_node) => {
                let (endpoint, (_, factory)) = io_node.as_parts_mut();
                let result = action(endpoint, factory)?;
                Ok(Some(result))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::dns::{DnsQuery, DnsResolver};
    use crate::service::select::Selectable;
    use std::cell::Cell;
    use std::rc::Rc;

    struct TestEndpoint {
        id: u32,
        fail: bool,
    }

    impl Selectable for TestEndpoint {
        fn connected(&mut self) -> io::Result<bool> {
            Ok(true)
        }

        fn make_writable(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn make_readable(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestSelector {
        next_token: SelectorToken,
    }

    impl Selector for TestSelector {
        type Target = TestEndpoint;

        fn register<F>(&mut self, _token: SelectorToken, _node: &mut IONode<Self::Target, F>) -> io::Result<()> {
            Ok(())
        }

        fn unregister<F>(&mut self, _node: &mut IONode<Self::Target, F>) -> io::Result<()> {
            Ok(())
        }

        fn poll<F>(&mut self, _nodes: &mut IONodes<Self::Target, F>) -> io::Result<()> {
            Ok(())
        }

        fn next_token(&mut self) -> SelectorToken {
            let token = self.next_token;
            self.next_token += 1;
            token
        }
    }

    struct FixedDns;
    struct FixedQuery;

    impl DnsResolver for FixedDns {
        type Query = FixedQuery;

        fn new_query(&self, _host: impl AsRef<str>, _port: u16) -> io::Result<Self::Query> {
            Ok(FixedQuery)
        }
    }

    impl DnsQuery for FixedQuery {
        fn poll(&mut self) -> io::Result<impl IntoIterator<Item = SocketAddr>> {
            Ok([SocketAddr::from(([127, 0, 0, 1], 1234))])
        }
    }

    #[derive(Clone)]
    struct ManualTime(Rc<Cell<u64>>);

    impl TimeSource for ManualTime {
        fn current_time_nanos(&self) -> u64 {
            self.0.get()
        }
    }

    struct TestEndpointFactory {
        id: u32,
        connection_info: crate::stream::ConnectionInfo,
        fail_poll: bool,
        recreate: bool,
    }

    impl TestEndpointFactory {
        fn new(id: u32) -> Self {
            Self {
                id,
                connection_info: crate::stream::ConnectionInfo::new("localhost", 1234),
                fail_poll: false,
                recreate: true,
            }
        }

        fn terminal(id: u32) -> Self {
            Self {
                fail_poll: true,
                recreate: false,
                ..Self::new(id)
            }
        }
    }

    impl ConnectionInfoProvider for TestEndpointFactory {
        fn connection_info(&self) -> &crate::stream::ConnectionInfo {
            &self.connection_info
        }
    }

    impl EndpointFactory for TestEndpointFactory {
        type Context = ();
        type Endpoint = TestEndpoint;

        fn create_endpoint(
            &mut self,
            _addr: SocketAddr,
            _ctx: &mut Self::Context,
        ) -> io::Result<Option<Self::Endpoint>> {
            Ok(Some(TestEndpoint {
                id: self.id,
                fail: self.fail_poll,
            }))
        }

        fn can_recreate(&mut self, _reason: &DisconnectReason, _ctx: &mut Self::Context) -> bool {
            self.recreate
        }
    }

    fn service(time: ManualTime) -> IOService<TestSelector, TestEndpointFactory, ManualTime, FixedDns> {
        IOService::new(TestSelector::default(), time, FixedDns)
    }

    fn connect_next(
        service: &mut IOService<TestSelector, TestEndpointFactory, ManualTime, FixedDns>,
        now: &Rc<Cell<u64>>,
        time_ns: u64,
    ) {
        now.set(time_ns);
        let events = service.poll(&mut ()).unwrap().collect::<Vec<_>>();
        assert!(matches!(events.as_slice(), [IOServiceEvent::Connected { .. }]));
    }

    #[derive(Default)]
    struct LifecycleContext {
        created: usize,
        auto_disconnect_checks: usize,
        reconnect_checks: usize,
        allow_auto_disconnect: bool,
        allow_reconnect: bool,
        processed: usize,
    }

    struct ContextEndpointFactory(TestEndpointFactory);

    impl ConnectionInfoProvider for ContextEndpointFactory {
        fn connection_info(&self) -> &crate::stream::ConnectionInfo {
            self.0.connection_info()
        }
    }

    impl EndpointFactory for ContextEndpointFactory {
        type Endpoint = TestEndpoint;
        type Context = LifecycleContext;

        fn create_endpoint(&mut self, addr: SocketAddr, ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {
            ctx.created += 1;
            self.0.create_endpoint(addr, &mut ())
        }

        fn can_auto_disconnect(&mut self, ctx: &mut Self::Context) -> bool {
            ctx.auto_disconnect_checks += 1;
            ctx.allow_auto_disconnect
        }

        fn can_recreate(&mut self, _reason: &DisconnectReason, ctx: &mut Self::Context) -> bool {
            ctx.reconnect_checks += 1;
            ctx.allow_reconnect
        }
    }

    #[test]
    fn context_controls_lifecycle_and_remains_available_while_processing_events() {
        let now = Rc::new(Cell::new(1));
        let mut service = IOService::new(TestSelector::default(), ManualTime(now.clone()), FixedDns)
            .with_auto_disconnect(Duration::from_secs(2));
        let handle = service
            .register(ContextEndpointFactory(TestEndpointFactory::new(7)))
            .unwrap();
        let mut ctx = LifecycleContext {
            allow_reconnect: true,
            ..LifecycleContext::default()
        };
        assert!(matches!(
            service.poll(&mut ctx).unwrap().next(),
            Some(IOServiceEvent::Connected { handle: h }) if h == handle
        ));
        assert_eq!(ctx.created, 1);

        // Context can defer an expired endpoint's automatic disconnection.
        now.set(2_000_000_002);
        let events: IOServiceEvents<'_, ContextEndpointFactory> = service.poll(&mut ctx).unwrap();
        assert_eq!(ctx.auto_disconnect_checks, 1);
        for event in events {
            let IOServiceEvent::Active(active) = event else {
                panic!("expected deferred endpoint to remain active");
            };
            active
                .try_with(|endpoint| {
                    assert_eq!(endpoint.id, 7);
                    ctx.processed += 1;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(ctx.processed, 1);
        assert_eq!(ctx.reconnect_checks, 0);

        // Dispatch captures the same context without a service context argument.
        assert_eq!(
            service
                .dispatch(handle, |endpoint, _factory| {
                    ctx.processed += 1;
                    Ok(endpoint.id)
                })
                .unwrap(),
            Some(7)
        );
        assert_eq!(ctx.processed, 2);

        ctx.allow_auto_disconnect = true;
        now.set(4_000_000_002);
        assert!(matches!(
            service.poll(&mut ctx).unwrap().next(),
            Some(IOServiceEvent::Disconnected {
                handle: h,
                reason: DisconnectReason::AutoDisconnect(_),
            }) if h == handle
        ));
        assert_eq!(ctx.auto_disconnect_checks, 2);
        assert_eq!(ctx.reconnect_checks, 1);
        assert!(matches!(
            service.poll(&mut ctx).unwrap().next(),
            Some(IOServiceEvent::Connected { handle: h }) if h == handle
        ));
        assert_eq!(ctx.created, 2);

        // A later application error also consults the same lifecycle context.
        for event in service.poll(&mut ctx).unwrap() {
            if let IOServiceEvent::Active(active) = event {
                ctx.allow_reconnect = false;
                assert!(active.try_with::<()>(|_| Err(io::Error::other("failed"))).is_err());
            }
        }
        assert!(matches!(
            service.poll(&mut ctx),
            Err(IOServiceError::EndpointNotRecreatable {
                handle: h,
                reason: DisconnectReason::IO(_),
            }) if h == handle
        ));
        assert_eq!(ctx.reconnect_checks, 2);
    }

    #[test]
    fn poll_visits_every_active_endpoint_once() {
        let now = Rc::new(Cell::new(1));
        let mut service = service(ManualTime(now.clone()));
        for id in 0..3 {
            service.register(TestEndpointFactory::new(id)).unwrap();
        }

        connect_next(&mut service, &now, 1);
        connect_next(&mut service, &now, 1_000_000_002);
        connect_next(&mut service, &now, 2_000_000_003);

        let events = service
            .poll(&mut ())
            .unwrap()
            .filter_map(|event| match event {
                IOServiceEvent::Active(active) => {
                    Some(active.try_with(|endpoint| Ok(endpoint.id)).unwrap().into_inner())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(events, [0, 1, 2]);
    }

    #[test]
    fn poll_skips_deregistered_slots() {
        let now = Rc::new(Cell::new(1));
        let mut service = service(ManualTime(now.clone()));
        let handles = (0..3)
            .map(|id| service.register(TestEndpointFactory::new(id)).unwrap())
            .collect::<Vec<_>>();

        connect_next(&mut service, &now, 1);
        connect_next(&mut service, &now, 1_000_000_002);
        connect_next(&mut service, &now, 2_000_000_003);

        service.deregister(handles[1]).unwrap();
        let events = service
            .poll(&mut ())
            .unwrap()
            .filter_map(|event| match event {
                IOServiceEvent::Active(active) => {
                    Some(active.try_with(|endpoint| Ok(endpoint.id)).unwrap().into_inner())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(events, [0, 2]);
    }

    #[test]
    fn returns_error_when_disconnected_endpoint_declines_recreation() {
        let now = Rc::new(Cell::new(1));
        let mut service = service(ManualTime(now.clone()));
        let handle = service.register(TestEndpointFactory::terminal(7)).unwrap();
        connect_next(&mut service, &now, 1);

        let mut events = service.poll(&mut ()).unwrap();
        let active = events
            .find_map(|event| match event {
                IOServiceEvent::Active(active) => Some(active),
                _ => None,
            })
            .expect("active endpoint");
        let source = active
            .try_with::<()>(|endpoint| {
                assert!(endpoint.fail);
                Err(io::Error::new(ErrorKind::ConnectionReset, "test disconnect"))
            })
            .unwrap_err();
        assert_eq!(source.kind(), ErrorKind::ConnectionReset);
        drop(events);

        let error = match service.poll(&mut ()) {
            Err(error) => error,
            Ok(_) => panic!("expected terminal endpoint error"),
        };
        match error {
            IOServiceError::EndpointNotRecreatable {
                handle: error_handle,
                reason: DisconnectReason::IO(source),
            } => {
                assert_eq!(error_handle, handle);
                assert_eq!(source.kind(), ErrorKind::ConnectionReset);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(service.iter().count(), 0);
    }

    #[test]
    fn deregister_clears_a_queued_disconnect() {
        let now = Rc::new(Cell::new(1));
        let mut service = service(ManualTime(now.clone()));
        let handle = service.register(TestEndpointFactory::terminal(7)).unwrap();
        connect_next(&mut service, &now, 1);

        let mut events = service.poll(&mut ()).unwrap();
        let active = events
            .find_map(|event| match event {
                IOServiceEvent::Active(active) => Some(active),
                _ => None,
            })
            .expect("active endpoint");
        assert!(
            active
                .try_with::<()>(|_| Err(io::Error::other("test disconnect")))
                .is_err()
        );
        drop(events);
        assert!(service.deregister(handle).unwrap().is_some());
        assert_eq!(service.poll(&mut ()).unwrap().count(), 0);
    }

    #[test]
    fn iterator_error_starts_the_reconnect_lifecycle() {
        let now = Rc::new(Cell::new(1));
        let mut service = service(ManualTime(now.clone()));
        let handle = service.register(TestEndpointFactory::new(7)).unwrap();
        connect_next(&mut service, &now, 1);

        let mut events = service.poll(&mut ()).unwrap();
        let active = events
            .find_map(|event| match event {
                IOServiceEvent::Active(active) => Some(active),
                _ => None,
            })
            .expect("active endpoint");
        let mut output = active
            .try_with(|_| Ok([Ok(7), Err(io::Error::new(ErrorKind::ConnectionAborted, "batch failed"))].into_iter()))
            .unwrap();
        assert_eq!(output.next().unwrap().unwrap(), 7);
        assert_eq!(output.next().unwrap().unwrap_err().kind(), ErrorKind::ConnectionAborted);
        drop(output);
        drop(events);

        now.set(2_000_000_002);
        let events = service.poll(&mut ()).unwrap().collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        let disconnected = events.into_iter().find_map(|event| match event {
            IOServiceEvent::Disconnected {
                handle: event_handle,
                reason: DisconnectReason::IO(source),
            } => Some((event_handle, source.kind())),
            IOServiceEvent::Connected { .. } | IOServiceEvent::Active(_) => {
                panic!("an endpoint must produce at most one event per service poll")
            }
            IOServiceEvent::Disconnected { .. } => None,
        });
        assert_eq!(disconnected, Some((handle, ErrorKind::ConnectionAborted)));
        assert!(service.poll(&mut ()).unwrap().any(
            |event| matches!(event, IOServiceEvent::Connected { handle: event_handle } if event_handle == handle)
        ));
    }
}
