use std::time::Duration;

use crate::util::current_time_nanos;

pub struct IONode<S, E> {
    pub stream: S,
    pub endpoint: E,
    pub disconnect_time_ns: u64,
}

impl<S, E> IONode<S, E> {
    pub fn new(stream: S, endpoint: E, ttl: Option<Duration>) -> IONode<S, E> {
        let disconnect_time_ns = match ttl {
            Some(ttl) => current_time_nanos() + ttl.as_nanos() as u64,
            None => u64::MAX,
        };
        Self {
            stream,
            endpoint,
            disconnect_time_ns,
        }
    }

    pub fn as_parts(&self) -> (&S, &E) {
        (&self.stream, &self.endpoint)
    }

    pub fn as_parts_mut(&mut self) -> (&mut S, &mut E) {
        (&mut self.stream, &mut self.endpoint)
    }

    pub fn as_stream(&mut self) -> &S {
        &self.stream
    }

    pub fn as_stream_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    pub fn as_endpoint(&self) -> &E {
        &self.endpoint
    }

    pub fn as_endpoint_mut(&mut self) -> &mut E {
        &mut self.endpoint
    }
}
