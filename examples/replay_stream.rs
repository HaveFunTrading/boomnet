#![allow(deprecated)]

use idle::IdleStrategy;
use std::time::Duration;

use boomnet::stream::replay::ReplayStream;
use boomnet::ws::{IntoWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = ReplayStream::from_file("plain_inbound.rec")?.into_websocket("wss://stream.binance.com:9443/ws");

    let idle = IdleStrategy::Sleep(Duration::from_millis(1));

    'outer: loop {
        'inner: loop {
            match ws.receive_next() {
                Ok(Some(WebsocketFrame::Text(fin, data))) => {
                    println!("({fin}) {}", String::from_utf8_lossy(data));
                }
                Ok(None) => break 'inner,
                Err(err) => {
                    println!("{}", err);
                    break 'outer;
                }
                _ => {}
            }
            idle.idle(0);
        }
    }

    Ok(())
}
