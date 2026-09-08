use std::io;
use std::marker::PhantomData;
use std::time::Duration;

use mio::event::Source;
use mio::{Events, Interest, Poll, Token};

use crate::service::dns::BlockingDnsResolver;
use crate::service::endpoint::EndpointFactory;
use crate::service::node::{IONode, IONodes};
use crate::service::select::{Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService};

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

    fn register<F>(&mut self, selector_token: SelectorToken, io_node: &mut IONode<Self::Target, F>) -> io::Result<()> {
        let token = Token(selector_token as usize);
        self.poll
            .registry()
            .register(io_node.as_endpoint_mut(), token, Interest::WRITABLE)?;
        Ok(())
    }

    fn unregister<F>(&mut self, io_node: &mut IONode<Self::Target, F>) -> io::Result<()> {
        self.poll.registry().deregister(io_node.as_endpoint_mut())
    }

    fn poll<F>(&mut self, io_nodes: &mut IONodes<Self::Target, F>) -> io::Result<()> {
        self.poll.poll(&mut self.events, NO_WAIT)?;
        for ev in self.events.iter() {
            let token = ev.token();
            let endpoint = io_nodes
                .get_mut(token.0 as SelectorToken)
                .ok_or_else(|| io::Error::other("io node not found"))?
                .as_endpoint_mut();
            if ev.is_writable() && endpoint.connected()? {
                endpoint.make_writable()?;
                self.poll.registry().reregister(endpoint, token, Interest::READABLE)?;
            }
            if ev.is_readable() {
                endpoint.make_readable()?;
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

impl<F: EndpointFactory> IntoIOService<F> for MioSelector<F::Endpoint> {
    fn into_io_service(self) -> IOService<Self, F, SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, SystemTimeClockSource, BlockingDnsResolver)
    }
}
