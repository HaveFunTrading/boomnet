use std::io;
use std::marker::PhantomData;

use crate::service::dns::BlockingDnsResolver;
use crate::service::endpoint::EndpointFactory;
use crate::service::select::{ActiveEndpointLookup, Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService};

pub struct DirectSelector<S> {
    phantom: PhantomData<S>,
}

impl<S> DirectSelector<S> {
    pub fn new() -> io::Result<DirectSelector<S>> {
        Ok(Self { phantom: PhantomData })
    }
}

impl<S: Selectable> Selector for DirectSelector<S> {
    type Target = S;

    fn register(&mut self, _token: SelectorToken, _endpoint: &mut Self::Target) -> io::Result<()> {
        Ok(())
    }

    fn unregister(&mut self, _token: SelectorToken, _endpoint: &mut Self::Target) -> io::Result<()> {
        Ok(())
    }

    fn poll(&mut self, _endpoints: &mut impl ActiveEndpointLookup<Self::Target>) -> io::Result<()> {
        Ok(())
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
