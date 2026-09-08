use crate::common::{TradeEndpoint, process_active};
use boomnet::service::dns::AsyncDnsResolver;
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};
use std::time::Duration;

#[path = "common/mod.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?
        .into_io_service()
        .with_auto_disconnect(Duration::from_secs(10))
        .with_dns_resolver(AsyncDnsResolver::new()?);

    let endpoint_btc_0 = TradeEndpoint::new("wss://stream1.binance.com:443/ws", None, "btcusdt");
    let endpoint_btc_1 = TradeEndpoint::new("wss://stream1.binance.com:443/ws", None, "btcusdt");
    let endpoint_btc_2 = TradeEndpoint::new("wss://stream1.binance.com:443/ws", None, "btcusdt");

    io_service.register(endpoint_btc_0)?;
    io_service.register(endpoint_btc_1)?;
    io_service.register(endpoint_btc_2)?;

    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                process_active(active)?;
            }
        }
    }
}
