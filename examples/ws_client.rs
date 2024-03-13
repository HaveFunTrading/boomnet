use std::net::TcpStream;
use std::time::Duration;

use boomnet::idle::IdleStrategy;
use boomnet::ws::{IntoTlsWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = TcpStream::connect("stream.binance.com:9443")?.into_tls_websocket("wss://stream.binance.com:9443/ws");

    ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;

    'outer: loop {
        let idle = IdleStrategy::Sleep(Duration::from_millis(1));

        'inner: loop {
            let mut wc = 0;
            match ws.receive_next() {
                Ok(Some(WebsocketFrame::Text(ts, fin, data))) => {
                    println!("{ts}: ({fin}) {}", String::from_utf8_lossy(data));
                    wc += 1;
                }
                Ok(None) => break 'inner,
                Err(err) => {
                    println!("{}", err);
                    break 'outer;
                }
                _ => {}
            }
            idle.idle(wc);
        }
    }

    Ok(())
}
