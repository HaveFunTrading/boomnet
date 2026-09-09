use std::io;
use std::marker::PhantomData;
use std::time::Duration;

use mio::event::Source;
use mio::{Events, Interest, Poll, Token};

use crate::service::dns::BlockingDnsResolver;
use crate::service::endpoint::EndpointFactory;
use crate::service::select::{ActiveEndpointLookup, Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService};

const NO_WAIT: Option<Duration> = Some(Duration::from_millis(0));

pub struct MioSelector<S> {
    poll: Poll,
    events: Events,
    phantom: PhantomData<S>,
}

impl<S> MioSelector<S> {
    pub fn new() -> io::Result<MioSelector<S>> {
        Ok(Self {
            poll: Poll::new()?,
            events: Events::with_capacity(1024),
            phantom: PhantomData,
        })
    }
}

impl<S: Source + Selectable> Selector for MioSelector<S> {
    type Target = S;

    fn register(&mut self, selector_token: SelectorToken, endpoint: &mut Self::Target) -> io::Result<()> {
        let token = Token(
            usize::try_from(selector_token)
                .map_err(|_| io::Error::other("selector token exceeds platform capacity"))?,
        );
        self.poll.registry().register(endpoint, token, Interest::WRITABLE)?;
        Ok(())
    }

    fn unregister(&mut self, _token: SelectorToken, endpoint: &mut Self::Target) -> io::Result<()> {
        self.poll.registry().deregister(endpoint)
    }

    fn poll(&mut self, endpoints: &mut impl ActiveEndpointLookup<Self::Target>) -> io::Result<()> {
        self.poll.poll(&mut self.events, NO_WAIT)?;
        for ev in self.events.iter() {
            let token = ev.token();
            let Some(endpoint) = endpoints.get_active_mut(token.0 as SelectorToken) else {
                continue;
            };
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
