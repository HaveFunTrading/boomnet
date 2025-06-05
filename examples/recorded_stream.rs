use boomnet::stream::ConnectionInfo;
use boomnet::stream::record::IntoRecordedStream;
use boomnet::stream::tls::IntoTlsStream;
use boomnet::ws::{IntoWebsocket, WebsocketFrame};
use idle::IdleStrategy;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let mut ws = ConnectionInfo::new("stream.binance.com", 9443)
        .into_tcp_stream()?
        .into_tls_stream()?
        .into_default_recorded_stream()
        .into_websocket("/ws");

    ws.send_text(true, Some(r#"{"method":"SUBSCRIBE","params":["btcusdt@trade"],"id":1}"#.to_string().as_bytes()))?;

    let idle = IdleStrategy::Sleep(Duration::from_millis(1));

    loop {
        for frame in ws.read_batch()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                println!("({fin}) {}", String::from_utf8_lossy(body));
            }
        }
        idle.idle(0);
    }
}
