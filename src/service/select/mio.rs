use std::collections::HashMap;
use std::io;
use std::marker::PhantomData;
use std::time::Duration;

use mio::event::Source;
use mio::{Events, Interest, Poll, Token};

use crate::service::endpoint::{Context, Endpoint, EndpointWithContext};
use crate::service::node::IONode;
use crate::service::select::{Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService, IntoIOServiceWithContext};

const NO_WAIT: Option<Duration> = Some(Duration::from_millis(0));

pub struct MioSelector<S> {
    poll: Poll,
    events: Events,
    next_token: u32,
    phantom: PhantomData<S>,
}

impl<S> MioSelector<S> {
    pub fn new() -> io::Result<MioSelector<S>> {
        Ok(Self {
            poll: Poll::new()?,
            events: Events::with_capacity(1024),
            next_token: 0,
            phantom: PhantomData,
        })
    }
}

impl<S: Source + Selectable> Selector for MioSelector<S> {
    type Target = S;

    fn register<E>(&mut self, selector_token: SelectorToken, io_node: &mut IONode<Self::Target, E>) -> io::Result<()> {
        let token = Token(selector_token as usize);
        self.poll
            .registry()
            .register(io_node.as_stream_mut(), token, Interest::WRITABLE)?;
        Ok(())
    }

    fn unregister<E>(&mut self, io_node: &mut IONode<Self::Target, E>) -> io::Result<()> {
        self.poll.registry().deregister(io_node.as_stream_mut())
    }

    fn poll<E>(&mut self, io_nodes: &mut HashMap<SelectorToken, IONode<Self::Target, E>>) -> io::Result<()> {
        self.poll.poll(&mut self.events, NO_WAIT)?;
        for ev in self.events.iter() {
            let token = ev.token();
            let stream = io_nodes
                .get_mut(&(token.0 as SelectorToken))
                .ok_or_else(|| io::Error::other("io node not found"))?
                .as_stream_mut();
            if ev.is_writable() && stream.connected()? {
                stream.make_writable()?;
                self.poll.registry().reregister(stream, token, Interest::READABLE)?;
            }
            if ev.is_readable() {
                stream.make_readable()?;
            }
        }
        Ok(())
    }

    #[inline]
    fn next_token(&mut self) -> SelectorToken {
        let token = self.next_token;
        self.next_token += 1;
        token
    }
}

impl<E: Endpoint> IntoIOService<E> for MioSelector<E::Target> {
    fn into_io_service(self) -> IOService<Self, E, ()>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, SystemTimeClockSource)
    }
}

impl<C: Context, E: EndpointWithContext<C>> IntoIOServiceWithContext<E, C> for MioSelector<E::Target> {
    fn into_io_service_with_context(self, _context: &mut C) -> IOService<Self, E, C>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, SystemTimeClockSource)
    }
}
