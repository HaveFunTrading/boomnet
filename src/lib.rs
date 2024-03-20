pub mod endpoint;
pub mod idle;
pub mod select;
pub mod service;
pub mod stream;
mod util;

mod buffer;
pub mod inet;
#[cfg(feature = "ws")]
pub mod ws;

pub struct IONode<S, E> {
    stream: S,
    endpoint: E,
}

impl<S, E> IONode<S, E> {
    pub fn new(stream: S, endpoint: E) -> IONode<S, E> {
        Self { stream, endpoint }
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
