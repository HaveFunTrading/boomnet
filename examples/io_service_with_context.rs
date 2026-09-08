//! Share lifecycle counters across endpoints and use the same context while processing events.

use std::io;
use std::net::SocketAddr;

use crate::common::{TradeEndpointFactory, process_active};
use boomnet::service::endpoint::{DisconnectReason, EndpointFactory};
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};

#[path = "common/mod.rs"]
mod common;

#[derive(Default, Debug)]
struct FeedContext {
    endpoints_created: usize,
    disconnects: usize,
    active_events: usize,
}

struct TrackedEndpointFactory(TradeEndpointFactory);

impl ConnectionInfoProvider for TrackedEndpointFactory {
    fn connection_info(&self) -> &ConnectionInfo {
        self.0.connection_info()
    }
}

impl EndpointFactory for TrackedEndpointFactory {
    type Endpoint = <TradeEndpointFactory as EndpointFactory>::Endpoint;
    type Context = FeedContext;

    fn create_endpoint(&mut self, addr: SocketAddr, ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {
        let endpoint = self.0.create_endpoint(addr, &mut ())?;
        if endpoint.is_some() {
            ctx.endpoints_created += 1;
        }
        Ok(endpoint)
    }

    fn can_recreate(&mut self, reason: &DisconnectReason, ctx: &mut Self::Context) -> bool {
        ctx.disconnects += 1;
        self.0.can_recreate(reason, &mut ())
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut ctx = FeedContext::default();
    let mut io_service = MioSelector::new()?.into_io_service();
    for instrument in ["btcusdt", "ethusdt", "xrpusdt"] {
        io_service.register(TrackedEndpointFactory(TradeEndpointFactory::new(
            "wss://stream.binance.com:443/ws",
            None,
            instrument,
        )))?;
    }

    loop {
        for event in io_service.poll(&mut ctx)? {
            if let IOServiceEvent::Active(active) = event {
                // Polling releases the context borrow before events are consumed.
                ctx.active_events += 1;
                process_active(active)?;
            } else {
                log::info!("lifecycle counters: {ctx:?}");
            }
        }
    }
}
