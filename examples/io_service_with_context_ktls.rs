#[cfg(feature = "ktls")]
mod deps {
    pub use boomnet::service::IntoIOService;
    pub use boomnet::service::endpoint::{DisconnectReason, Endpoint};
    pub use boomnet::service::select::Selectable;
    pub use boomnet::service::select::mio::MioSelector;
    pub use boomnet::stream::ktls::{IntoKtlsStream, KtlStream};
    pub use boomnet::stream::mio::{IntoMioStream, MioStream};
    pub use boomnet::stream::tcp::TcpStream;
    pub use boomnet::stream::tls::TlsConfigExt;
    pub use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
    pub use boomnet::ws::{IntoWebsocket, Websocket, WebsocketFrame};
    pub use mio::event::Source;
    pub use mio::{Interest, Registry, Token};
    pub use std::net::SocketAddr;
    pub use std::time::Duration;
}

#[cfg(feature = "ktls")]
use deps::*;

#[cfg(feature = "ktls")]
struct TradeConnectionFactory {
    connection_info: ConnectionInfo,
}

#[cfg(feature = "ktls")]
impl TradeConnectionFactory {
    fn new() -> Self {
        Self {
            connection_info: ("fstream.binance.com", 443).into(),
        }
    }
}

#[cfg(feature = "ktls")]
struct TradeConnection {
    ws: Websocket<KtlStream<MioStream>>,
}

#[cfg(feature = "ktls")]
impl TradeConnection {
    fn do_work(&mut self) -> std::io::Result<()> {
        for frame in self.ws.read_batch()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                println!("({fin}) {}", String::from_utf8_lossy(body));
            }
        }
        Ok(())
    }
}

#[cfg(feature = "ktls")]
impl Selectable for TradeConnection {
    fn connected(&mut self) -> std::io::Result<bool> {
        self.ws.connected()
    }

    fn make_writable(&mut self) -> std::io::Result<()> {
        self.ws.make_writable()
    }

    fn make_readable(&mut self) -> std::io::Result<()> {
        self.ws.make_readable()
    }
}

#[cfg(feature = "ktls")]
impl Source for TradeConnection {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> std::io::Result<()> {
        self.ws.register(registry, token, interests)
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> std::io::Result<()> {
        self.ws.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &Registry) -> std::io::Result<()> {
        self.ws.deregister(registry)
    }
}

#[cfg(feature = "ktls")]
impl ConnectionInfoProvider for TradeConnectionFactory {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

#[cfg(feature = "ktls")]
impl Endpoint for TradeConnectionFactory {
    type Target = TradeConnection;

    fn create_target(&mut self, addr: SocketAddr) -> std::io::Result<Option<Self::Target>> {
        let mut ws = TcpStream::try_from((&self.connection_info, addr))?
            .into_mio_stream()
            .into_ktls_stream_with_config(|cfg| cfg.with_no_cert_verification())?
            .into_websocket("/ws");

        ws.send_text(true, Some(b"{\"method\":\"SUBSCRIBE\",\"params\":[\"btcusdt@trade\"],\"id\":1}"))?;

        Ok(Some(TradeConnection { ws }))
    }

    fn can_recreate(&mut self, reason: DisconnectReason) -> bool {
        println!("on disconnect: reason={}", reason);
        true
    }
}

#[cfg(feature = "ktls")]
fn main() -> anyhow::Result<()> {
    let mut io_service = MioSelector::new()?
        .into_io_service()
        .with_auto_disconnect(Duration::from_secs(10));

    io_service.register(TradeConnectionFactory::new())?;

    loop {
        io_service.poll(|conn, _| conn.do_work())?;
    }
}

#[cfg(not(feature = "ktls"))]
fn main() {}
