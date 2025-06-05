use crate::common::TradeEndpoint;
use boomnet::service::IntoIOService;
use boomnet::service::select::mio::MioSelector;

#[path = "common/mod.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?.into_io_service();

    let endpoint_xrp = TradeEndpoint::new_with_subscribe(2, "wss://stream3.binance.com:443/ws", None, "xrpusdt", false);

    let handle = io_service.register(endpoint_xrp);

    // we delay the subscription until the endpoint is ready
    loop {
        let success = io_service.dispatch(handle, |ws, endpoint| {
            endpoint.subscribe(ws)?;
            Ok(())
        })?;
        if success {
            break;
        } else {
            io_service.poll()?;
        }
    }

    loop {
        io_service.poll()?;
    }
}
