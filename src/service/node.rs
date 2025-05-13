use crate::service::Handle;
use crate::util::current_time_nanos;
use std::time::Duration;

pub struct IONode<S, E> {
    pub stream: S,
    pub endpoint: Option<(Handle, E)>,
    pub disconnect_time_ns: u64,
}

impl<S, E> IONode<S, E> {
    pub fn new(stream: S, handle: Handle, endpoint: E, ttl: Option<Duration>) -> IONode<S, E> {
        let disconnect_time_ns = match ttl {
            Some(ttl) => current_time_nanos() + ttl.as_nanos() as u64,
            None => u64::MAX,
        };
        Self {
            stream,
            endpoint: Some((handle, endpoint)),
            disconnect_time_ns,
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
}
