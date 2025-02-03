use boomnet::stream::tcp::TcpStream;
use boomnet::ws::{Websocket, WebsocketFrame};
use idle::IdleStrategy;
use openssl::ssl::{HandshakeError, SslConnector, SslMethod, SslRef};
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;
// use native_tls::{HandshakeError, TlsConnector, TlsStream};

fn key_log_callback(_ssl: &SslRef, line: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("sslkeylog.log")
        .expect("Failed to open SSL key log file");

    writeln!(file, "{}", line).expect("Failed to write to SSL key log file");
}

fn main() -> anyhow::Result<()> {
    let stream = TcpStream::try_from(("stream.binance.com", 9443))?;

    let mut builder = SslConnector::builder(SslMethod::tls()).expect("Failed to create SSL connector");
    builder.set_keylog_callback(key_log_callback); // Set the key logging callback

    let connector = builder.build();
    // let stream = connector.connect("stream.binance.com", stream)?;

    // let connector = TlsConnector::new()?;

    // Perform TLS handshake manually
    let stream = match connector.connect("stream.binance.com", stream) {
        Ok(stream) => stream,
        Err(HandshakeError::WouldBlock(mut mid_handshake)) => {
            // Poll the socket until handshake is complete
            loop {
                match mid_handshake.handshake() {
                    Ok(ssl_stream) => {
                        println!("TLS Handshake successful");
                        break ssl_stream;
                    }
                    Err(HandshakeError::WouldBlock(mid)) => {
                        mid_handshake = mid;
                        std::thread::sleep(Duration::from_millis(10)); // Avoid busy-waiting
                    }
                    Err(e) => {
                        eprintln!("TLS Handshake failed: {:?}", e);
                        return Err(std::io::Error::new(std::io::ErrorKind::Other, "TLS handshake failed"))?;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("TLS handshake error: {:?}", e);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "TLS handshake failed"))?;
        }
    };

    let mut ws = Websocket::new(stream, "stream.binance.com", "/ws?timeUnit=microsecond");

    // let mut ws = TcpStream::try_from(("stream.binance.com", 9443))?
    //     .into_tls_stream()
    //     .into_websocket("/ws?timeUnit=microsecond");

    ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;

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
