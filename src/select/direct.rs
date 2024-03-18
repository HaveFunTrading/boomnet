use std::collections::HashMap;
use std::io;
use std::marker::PhantomData;

use crate::endpoint::{Context, Endpoint, EndpointWithContext};
use crate::idle::IdleStrategy;
use crate::select::{Selectable, Selector, SelectorToken};
use crate::service::{IOService, IntoIOService, IntoIOServiceWithContext};
use crate::IONode;

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

    fn register<E>(&mut self, _io_node: &mut IONode<Self::Target, E>) -> io::Result<SelectorToken> {
        let token = self.next_token;
        self.next_token += 1;
        Ok(token)
    }

    fn unregister<E>(&mut self, _io_node: &mut IONode<Self::Target, E>) -> io::Result<()> {
        Ok(())
    }

    fn poll<E>(&mut self, _io_nodes: &mut HashMap<SelectorToken, IONode<Self::Target, E>>) -> io::Result<()> {
        Ok(())
    }
}

impl<E: Endpoint> IntoIOService<E> for DirectSelector<E::Target> {
    fn into_io_service(self, idle_strategy: IdleStrategy) -> IOService<Self, E, ()>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, idle_strategy)
    }
}

impl<C: Context, E: EndpointWithContext<C>> IntoIOServiceWithContext<E, C> for DirectSelector<E::Target> {
    fn into_io_service_with_context(self, idle_strategy: IdleStrategy, _context: &mut C) -> IOService<Self, E, C>
    where
        Self: Selector,
        Self: Sized,
    {
        IOService::new(self, idle_strategy)
    }
}
