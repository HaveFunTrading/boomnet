use idle::IdleStrategy;
use std::time::Duration;

use boomnet::stream::ConnectionInfo;
use boomnet::ws::{IntoTlsWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = ConnectionInfo::new("stream.binance.com", 9443)
        .into_tcp_stream()?
        .into_tls_websocket("/ws");

    ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;

    let idle = IdleStrategy::Sleep(Duration::from_millis(1));

    loop {
        for frame in ws.batch_iter()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                println!("({fin}) {}", String::from_utf8_lossy(body));
            }
        }
        idle.idle(0);
    }
}
