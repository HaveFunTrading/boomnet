use boomnet::stream::BindAndConnect;
use boomnet::stream::tls::{IntoTlsStream, TlsConfigExt};
use boomnet::ws::{IntoWebsocket, WebsocketFrame};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let tcp_stream =
        std::net::TcpStream::bind_and_connect_with_socket_config("fstream.binance.com:443", None, None, |cfg| {
            cfg.set_nonblocking(false)
        })?;
    let tcp_stream = boomnet::stream::tcp::TcpStream::new(tcp_stream, ("fstream.binance.com", 443).into());
    let mut ws = tcp_stream
        .into_tls_stream_with_config(|cfg| cfg.with_no_cert_verification())?
        .into_websocket("/ws");

    ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"1000pepeusdt@depth@0ms\"],\"id\":1}"))?;

    loop {
        for frame in ws.read_batch()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                println!("({fin}) ({}) {}", body.len(), String::from_utf8_lossy(body));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
