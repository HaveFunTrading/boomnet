use idle::IdleStrategy;
use std::net::TcpStream;
use std::time::Duration;
use boomnet::stream::ConnectionInfo;
use boomnet::stream::record::IntoRecordedStream;
use boomnet::stream::tls::IntoTlsStream;
use boomnet::ws::{IntoWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = ConnectionInfo::new("stream.binance.com", 9443)
        .into_tcp_stream()?
        .into_tls_stream()
        .into_recorded_stream("plain")
        .into_websocket("/ws");

    ws.send_text(true, Some(r#"{"method":"SUBSCRIBE","params":["btcusdt@trade"],"id":1}"#.to_string().as_bytes()))?;

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
