use crate::common::{TradeEndpointFactory, process_active};
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};

#[path = "common/mod.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?.into_io_service();

    let factory_xrp =
        TradeEndpointFactory::new_with_subscribe("wss://stream3.binance.com:443/ws", None, "xrpusdt", false);

    let handle = io_service.register(factory_xrp)?;

    // we delay the subscription until the endpoint is ready
    loop {
        let success = io_service.dispatch(handle, |ws, factory| {
            factory.subscribe(ws)?;
            Ok(())
        })?;
        if success.is_some() {
            break;
        } else {
            for event in io_service.poll(&mut ())? {
                if let IOServiceEvent::Active(active) = event {
                    process_active(active)?;
                }
            }
        }
    }

    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                process_active(active)?;
            }
        }
    }
}
