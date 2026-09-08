use std::io;
use std::marker::PhantomData;

use crate::service::dns::BlockingDnsResolver;
use crate::service::endpoint::EndpointFactory;
use crate::service::node::{IONode, IONodes};
use crate::service::select::{Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService};

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

    fn register<F>(
        &mut self,
        _selector_token: SelectorToken,
        _io_node: &mut IONode<Self::Target, F>,
    ) -> io::Result<()> {
        Ok(())
    }

    fn unregister<F>(&mut self, _io_node: &mut IONode<Self::Target, F>) -> io::Result<()> {
        Ok(())
    }

    fn poll<F>(&mut self, _io_nodes: &mut IONodes<Self::Target, F>) -> io::Result<()> {
        Ok(())
    }

    fn next_token(&mut self) -> SelectorToken {
        let token = self.next_token;
        self.next_token += 1;
        token
    }
}

impl<F: EndpointFactory> IntoIOService<F> for DirectSelector<F::Endpoint> {
    fn into_io_service(self) -> IOService<Self, F, SystemTimeClockSource, BlockingDnsResolver>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, SystemTimeClockSource, BlockingDnsResolver)
    }
}
