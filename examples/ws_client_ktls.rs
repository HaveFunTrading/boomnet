use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{Websocket, WebsocketFrame};
use openssl::ssl::ErrorCode;
use std::io;
use std::io::{ErrorKind, Read, Write};

struct KtlsStream {
    inner: openssl_ktls::SslStream,
    conn: ConnectionInfo,
}

impl Read for KtlsStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for KtlsStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl ConnectionInfoProvider for KtlsStream {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.conn
    }
}

impl KtlsStream {
    fn connect(&self) -> io::Result<()> {
        match self.inner.connect() {
            Ok(_) => Ok(()),
            Err(err) if err.code() == ErrorCode::WANT_READ => Err(ErrorKind::WouldBlock.into()),
            Err(err) if err.code() == ErrorCode::WANT_WRITE => Err(ErrorKind::WouldBlock.into()),
            Err(err) => Err(io::Error::other(err)),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls_client())?;
    builder.set_options(openssl_ktls::option::SSL_OP_ENABLE_KTLS);
    // connector.set_cipher_list(openssl_ktls::option::ECDHE_RSA_AES128_GCM_SHA256)?;
    // connector.set_ciphersuites("TLS_AES_256_GCM_SHA384")?;
    let connector = builder.build();

    let tcp_stream = std::net::TcpStream::connect("stream.crypto.com:443")?;
    let ssl = connector.configure()?.into_ssl("stream.crypto.com")?;

    let ktls_stream = KtlsStream {
        inner: openssl_ktls::SslStream::new(tcp_stream, ssl),
        conn: ("stream.crypto.com", 443).into(),
    };

    ktls_stream.connect()?;

    assert!(ktls_stream.inner.ktls_recv_enabled());
    assert!(ktls_stream.inner.ktls_send_enabled());

    // let mut ws = Websocket::new(ktls_stream, "/ws");
    let mut ws = Websocket::new(ktls_stream, "/exchange/v1/market");

    // ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;
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
