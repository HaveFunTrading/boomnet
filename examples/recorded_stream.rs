use std::net::TcpStream;
use std::time::Duration;

use bnet::idle::IdleStrategy;
use bnet::stream::recorder::Record;
use bnet::stream::tls::IntoTlsStream;
use bnet::ws::{WebsocketFrame, IntoWebsocket};

fn main() -> anyhow::Result<()> {
    let mut ws = TcpStream::connect("stream.binance.com:9443")?
        .into_tls_stream("stream.binance.com")
        .record()
        .into_websocket("wss://stream.binance.com:9443/ws");

    ws.send_text(true, Some(r#"{"method":"SUBSCRIBE","params":["btcusdt@trade"],"id":1}"#.to_string().as_bytes()))?;

    let idle = IdleStrategy::Sleep(Duration::from_millis(1));

    'outer: loop {
        'inner: loop {
            match ws.receive_next() {
                Ok(Some(WebsocketFrame::Text(ts, fin, data))) => {
                    println!("{ts}: ({fin}) {}", String::from_utf8_lossy(data));
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
