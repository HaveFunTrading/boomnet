use crate::common::{FeedContext, TradeEndpoint};
use boomnet::service::select::mio::MioSelector;
use boomnet::service::IntoIOServiceWithContext;

#[path = "common/mod.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut context = FeedContext::new();

    let mut io_service = MioSelector::new()?.into_io_service_with_context(&mut context);

    let endpoint_btc = TradeEndpoint::new(0, "wss://stream1.binance.com:443/ws", None, "btcusdt");
    let endpoint_eth = TradeEndpoint::new(1, "wss://stream2.binance.com:443/ws", None, "ethusdt");
    let endpoint_xrp = TradeEndpoint::new(2, "wss://stream3.binance.com:443/ws", None, "xrpusdt");

    io_service.register(endpoint_btc);
    io_service.register(endpoint_eth);
    io_service.register(endpoint_xrp);

    loop {
        io_service.poll(&mut context)?;
    }
}
