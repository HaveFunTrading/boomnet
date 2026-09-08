use crate::common::{TradeEndpointFactory, process_active};
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

    let factory_btc = TradeEndpointFactory::new("wss://stream1.binance.com:443/ws", None, "btcusdt");
    let factory_eth = TradeEndpointFactory::new("wss://stream2.binance.com:443/ws", None, "ethusdt");
    let factory_xrp = TradeEndpointFactory::new("wss://stream3.binance.com:443/ws", None, "xrpusdt");

    io_service.register(factory_btc)?;
    io_service.register(factory_eth)?;
    io_service.register(factory_xrp)?;

    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                process_active(active)?;
            }
        }
    }
}
