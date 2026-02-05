use boomnet::stream::tcp::TcpStream;
use boomnet::stream::tls::{IntoTlsStream, TlsConfigExt};
use boomnet::ws::{IntoWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = TcpStream::try_from(("ws-fapi.binance.com", 443))?
        .into_tls_stream_with_config(|cfg| cfg.with_no_cert_verification())?
        .into_websocket("/ws-fapi/v1");

    ws.send_ping(Some("test".as_bytes()))?;

    loop {
        if let Some(frame) = ws.receive_next() {
            if let WebsocketFrame::Pong(payload) = frame? {
                println!("Received pong: {}", String::from_utf8_lossy(payload));
                break;
            }
        }
    }

    Ok(())
}
