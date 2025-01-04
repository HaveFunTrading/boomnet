use idle::IdleStrategy;
use std::net::TcpStream;
use std::time::Duration;

use boomnet::stream::buffer::IntoBufferedStream;
use boomnet::stream::tls::IntoTlsStream;
use boomnet::stream::BindAndConnect;
use boomnet::ws::{IntoWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = TcpStream::bind_and_connect("stream.binance.com:9443", None, None)?
        .into_tls_stream("stream.binance.com")
        .into_default_buffered_stream()
        .into_websocket("wss://stream.binance.com:9443/ws");

    // websocket can also be quickly constructed from url string (use only for testing)
    // let mut ws = "wss://stream.binance.com:443/ws".try_into_tls_ready_websocket()?;

    ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;

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
