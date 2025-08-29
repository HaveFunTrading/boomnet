use crate::service::Handle;
use crate::service::time::TimeSource;
use std::net::SocketAddr;
use std::time::Duration;

pub struct IONode<S, E> {
    pub stream: S,
    pub endpoint: Option<(Handle, E)>,
    pub ttl: Duration,
    pub disconnect_time_ns: u64,
    pub addr: SocketAddr,
}

impl<S, E> IONode<S, E> {
    pub fn new<TS>(
        stream: S,
        handle: Handle,
        endpoint: E,
        ttl: Option<Duration>,
        ts: &TS,
        addr: SocketAddr,
    ) -> IONode<S, E>
    where
        TS: TimeSource,
    {
        let ttl = ttl.map_or(u64::MAX, |ttl| ttl.as_nanos() as u64);
        Self {
            stream,
            endpoint: Some((handle, endpoint)),
            ttl: Duration::from_nanos(ttl),
            disconnect_time_ns: ts.current_time_nanos().saturating_add(ttl),
            addr,
        }
    }

    pub fn as_parts(&self) -> (&S, &(Handle, E)) {
        // SAFETY: safe to call as endpoint will never be None
        unsafe { (&self.stream, self.endpoint.as_ref().unwrap_unchecked()) }
    }

    pub fn as_parts_mut(&mut self) -> (&mut S, &mut (Handle, E)) {
        // SAFETY: safe to call as endpoint will never be None
        unsafe { (&mut self.stream, self.endpoint.as_mut().unwrap_unchecked()) }
    }

    pub const fn as_stream(&self) -> &S {
        &self.stream
    }

    pub fn as_stream_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    pub fn as_endpoint(&self) -> &(Handle, E) {
        // SAFETY: safe to call as endpoint will never be None
        unsafe { self.endpoint.as_ref().unwrap_unchecked() }
    }

    pub fn as_endpoint_mut(&mut self) -> &mut (Handle, E) {
        // SAFETY: safe to call as endpoint will never be None
        unsafe { self.endpoint.as_mut().unwrap_unchecked() }
    }

    pub fn into_endpoint(mut self) -> (Handle, E) {
        // SAFETY: safe to call as endpoint will never be None
        unsafe { self.endpoint.take().unwrap_unchecked() }
    }
}
