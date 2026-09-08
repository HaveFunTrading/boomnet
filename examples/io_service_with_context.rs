//! Share lifecycle counters across endpoints and use the same context while processing events.

use std::io;
use std::net::SocketAddr;

use crate::common::{TradeEndpoint, process_active};
use boomnet::service::endpoint::{DisconnectReason, Endpoint};
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};

#[path = "common/mod.rs"]
mod common;

#[derive(Default, Debug)]
struct FeedContext {
    targets_created: usize,
    disconnects: usize,
    active_events: usize,
}

struct TrackedEndpoint(TradeEndpoint);

impl ConnectionInfoProvider for TrackedEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        self.0.connection_info()
    }
}

impl Endpoint for TrackedEndpoint {
    type Target = <TradeEndpoint as Endpoint>::Target;
    type Context = FeedContext;

    fn create_target(&mut self, addr: SocketAddr, ctx: &mut Self::Context) -> io::Result<Option<Self::Target>> {
        let target = self.0.create_target(addr, &mut ())?;
        if target.is_some() {
            ctx.targets_created += 1;
        }
        Ok(target)
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
        io_service.register(TrackedEndpoint(TradeEndpoint::new(
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
