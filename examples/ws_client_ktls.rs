use boomnet::stream::ktls::IntoKtlsStream;
use boomnet::stream::tcp::TcpStream;
use boomnet::stream::tls::TlsConfigExt;
use boomnet::ws::{IntoWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = TcpStream::try_from(("fstream.binance.com", 443))?
        .into_ktls_stream_with_config(|cfg| cfg.with_no_cert_verification())?
        .into_websocket("/ws");

    ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;

    loop {
        for frame in ws.read_batch()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                println!("({fin}) {}", String::from_utf8_lossy(body));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
