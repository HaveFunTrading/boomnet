use boomnet::ws::{Websocket, WebsocketFrame};
use boomnet::stream::ktls::KtlSteam;
use boomnet::stream::tcp::TcpStream;

fn main() -> anyhow::Result<()> {
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls_client())?;
    builder.set_options(openssl_ktls::option::SSL_OP_ENABLE_KTLS);
    let connector = builder.build();

    let tcp_stream = TcpStream::try_from(("stream.crypto.com", 443))?;
    let ssl = connector.configure()?.into_ssl("stream.crypto.com")?;
    let ktls_stream = KtlSteam::new(tcp_stream, ssl);

    ktls_stream.blocking_connect()?;

    println!("Connected!");

    assert!(ktls_stream.ktls_recv_enabled());
    assert!(ktls_stream.ktls_send_enabled());

    let mut ws = Websocket::new(ktls_stream, "/exchange/v1/market");

    ws.send_text(true, Some(br#"{"id":1,"method":"subscribe","params":{"channels":["trade.BTCUSD-PERP"]}}"#))?;

    // let idle = IdleStrategy::Sleep(Duration::from_millis(1));

    let mut log = false;

    loop {
        for frame in ws.read_batch()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                if !log {
                    println!("({fin}) {}", String::from_utf8_lossy(body));
                    // log = true;
                }
            }
        }
        // idle.idle(0);
    }
}
