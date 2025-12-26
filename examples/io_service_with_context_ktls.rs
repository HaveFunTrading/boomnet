use boomnet::service::IntoIOService;
use boomnet::service::endpoint::{DisconnectReason, Endpoint};
use boomnet::service::select::Selectable;
use boomnet::service::select::mio::MioSelector;
use boomnet::stream::ktls::{IntoKtlsStream, KtlStream};
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::tcp::TcpStream;
use boomnet::stream::tls::TlsConfigExt;
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoWebsocket, Websocket, WebsocketFrame};
use mio::event::Source;
use mio::{Interest, Registry, Token};
use std::net::SocketAddr;
use std::time::Duration;

struct TradeConnectionFactory {
    connection_info: ConnectionInfo,
}

impl TradeConnectionFactory {
    fn new() -> Self {
        Self {
            connection_info: ("fstream.binance.com", 443).into(),
        }
    }
}

struct TradeConnection {
    ws: Websocket<KtlStream<MioStream>>,
}

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

impl ConnectionInfoProvider for TradeConnectionFactory {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

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

fn main() -> anyhow::Result<()> {
    let mut io_service = MioSelector::new()?
        .into_io_service()
        .with_auto_disconnect(Duration::from_secs(10));

    io_service.register(TradeConnectionFactory::new())?;

    loop {
        io_service.poll(|conn, _| conn.do_work())?;
    }
}
