#[path = "common/mod.rs"]
mod common;

#[cfg(all(feature = "c-ares", feature = "mio"))]
mod deps {
    pub use crate::common::{TradeEndpoint, process_active};
    pub use boomnet::service::dns::CaresDnsResolver;
    pub use boomnet::service::select::mio::MioSelector;
    pub use boomnet::service::{IOServiceEvent, IntoIOService};
    pub use std::time::Duration;
}

#[cfg(all(feature = "c-ares", feature = "mio"))]
use deps::*;

#[cfg(all(feature = "c-ares", feature = "mio"))]
fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?
        .into_io_service()
        .with_auto_disconnect(Duration::from_secs(10))
        .with_dns_resolver(CaresDnsResolver::new());

    let endpoint_btc = TradeEndpoint::new("wss://stream1.binance.com:443/ws", None, "btcusdt");
    let endpoint_eth = TradeEndpoint::new("wss://stream2.binance.com:443/ws", None, "ethusdt");
    let endpoint_xrp = TradeEndpoint::new("wss://stream3.binance.com:443/ws", None, "xrpusdt");

    io_service.register(endpoint_btc)?;
    io_service.register(endpoint_eth)?;
    io_service.register(endpoint_xrp)?;

    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                process_active(active)?;
            }
        }
    }
}

#[cfg(not(all(feature = "c-ares", feature = "mio")))]
fn main() {}
