use boomnet::stream::record::IntoRecordedStream;
use boomnet::stream::tcp::TcpStream;
use boomnet::stream::tls::IntoTlsStream;
use boomnet::ws::{IntoWebsocket, WebsocketFrame};
use idle::IdleStrategy;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let mut ws = TcpStream::try_from(("stream.binance.com", 9443))?
        .into_tls_stream()
        .into_default_recorded_stream()
        .into_websocket("/ws?timeUnit=microsecond");

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
