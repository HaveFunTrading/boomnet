use crate::common::{TradeEndpoint, process_active};
use boomnet::service::dns::AsyncDnsResolver;
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};

#[path = "common/mod.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?
        .into_io_service()
        .with_dns_resolver(AsyncDnsResolver::new()?);

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
