use std::collections::HashMap;
use std::io;
use std::marker::PhantomData;

use crate::service::dns::BlockingDnsResolver;
use crate::service::endpoint::{Context, Endpoint, EndpointWithContext};
use crate::service::node::IONode;
use crate::service::select::{Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService, IntoIOServiceWithContext};

pub struct DirectSelector<S> {
    next_token: u32,
    phantom: PhantomData<S>,
}

impl<S> DirectSelector<S> {
    pub fn new() -> io::Result<DirectSelector<S>> {
        Ok(Self {
            next_token: 0,
            phantom: PhantomData,
        })
    }
}

impl<S: Selectable> Selector for DirectSelector<S> {
    type Target = S;

    fn register<E>(
        &mut self,
        _selector_token: SelectorToken,
        _io_node: &mut IONode<Self::Target, E>,
    ) -> io::Result<()> {
        Ok(())
    }

    fn unregister<E>(&mut self, _io_node: &mut IONode<Self::Target, E>) -> io::Result<()> {
        Ok(())
    }

    fn poll<E>(&mut self, _io_nodes: &mut HashMap<SelectorToken, IONode<Self::Target, E>>) -> io::Result<()> {
        Ok(())
    }

    fn next_token(&mut self) -> SelectorToken {
        let token = self.next_token;
        self.next_token += 1;
        token
    }
}

impl<E: Endpoint> IntoIOService<E> for DirectSelector<E::Target> {
    fn into_io_service(self) -> IOService<Self, E, (), SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, SystemTimeClockSource, BlockingDnsResolver)
    }
}

impl<C: Context, E: EndpointWithContext<C>> IntoIOServiceWithContext<E, C> for DirectSelector<E::Target> {
    fn into_io_service_with_context(
        self,
        _ctx: &mut C,
    ) -> IOService<Self, E, C, SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, SystemTimeClockSource, BlockingDnsResolver)
    }
}
