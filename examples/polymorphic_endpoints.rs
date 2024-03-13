#![allow(unused)]

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use log::info;

use boomnet::endpoint::ws::{TlsWebsocket, TlsWebsocketEndpointWithContext};
use boomnet::endpoint::Context;
use boomnet::idle::IdleStrategy;
use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
use boomnet::select::mio::MioSelector;
use boomnet::service::IntoIOServiceWithContext;
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::tls::TlsStream;
use boomnet::stream::BindAndConnect;
use boomnet::ws::{IntoTlsWebsocket, Websocket, WebsocketFrame};

struct FeedContext;

impl Context for FeedContext {}
enum MarketDataEndpoint {
    Trade(TradeEndpoint),
    Ticker(TickerEndpoint),
}

impl TlsWebsocketEndpointWithContext<FeedContext> for MarketDataEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        match self {
            MarketDataEndpoint::Ticker(ticker) => ticker.url(),
            MarketDataEndpoint::Trade(trade) => trade.url(),
        }
    }

    fn create_websocket(
        &self,
        addr: SocketAddr,
        context: &mut FeedContext,
    ) -> io::Result<Websocket<TlsStream<Self::Stream>>> {
        match self {
            MarketDataEndpoint::Ticker(ticker) => ticker.create_websocket(addr, context),
            MarketDataEndpoint::Trade(trade) => trade.create_websocket(addr, context),
        }
    }

    fn poll(&self, ws: &mut Websocket<TlsStream<Self::Stream>>, ctx: &mut FeedContext) -> io::Result<()> {
        match self {
            MarketDataEndpoint::Ticker(ticker) => TlsWebsocketEndpointWithContext::poll(ticker, ws, ctx),
            MarketDataEndpoint::Trade(trade) => TlsWebsocketEndpointWithContext::poll(trade, ws, ctx),
        }
    }
}

struct TradeEndpoint {
    id: u32,
    url: &'static str,
    net_iface: Option<SocketAddr>,
    instrument: &'static str,
}

impl TradeEndpoint {
    pub fn new(id: u32, url: &'static str, net_iface: Option<&'static str>, instrument: &'static str) -> TradeEndpoint {
        let net_iface = net_iface
            .and_then(|name| name.into_network_interface())
            .and_then(|iface| iface.to_socket_addr());
        Self {
            id,
            url,
            net_iface,
            instrument,
        }
    }
}

impl TlsWebsocketEndpointWithContext<FeedContext> for TradeEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        self.url
    }

    fn create_websocket(&self, addr: SocketAddr, _ctx: &mut FeedContext) -> io::Result<TlsWebsocket<Self::Stream>> {
        let mut ws = TcpStream::bind_and_connect(addr, self.net_iface, None)?
            .into_mio_stream()
            .into_tls_websocket(self.url);

        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&self, ws: &mut TlsWebsocket<Self::Stream>, _ctx: &mut FeedContext) -> io::Result<()> {
        while let Some(WebsocketFrame::Text(ts, fin, data)) = ws.receive_next()? {
            info!("{ts}: ({fin}) {}", String::from_utf8_lossy(data));
        }
        Ok(())
    }
}

struct TickerEndpoint {
    id: u32,
    url: &'static str,
    net_iface: Option<SocketAddr>,
    instrument: &'static str,
}

impl TickerEndpoint {
    pub fn new(
        id: u32,
        url: &'static str,
        net_iface: Option<&'static str>,
        instrument: &'static str,
    ) -> TickerEndpoint {
        let net_iface = net_iface
            .and_then(|name| name.into_network_interface())
            .and_then(|iface| iface.to_socket_addr());
        Self {
            id,
            url,
            net_iface,
            instrument,
        }
    }
}

impl TlsWebsocketEndpointWithContext<FeedContext> for TickerEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        self.url
    }

    fn create_websocket(&self, addr: SocketAddr, _ctx: &mut FeedContext) -> io::Result<TlsWebsocket<Self::Stream>> {
        let mut ws = TcpStream::bind_and_connect(addr, self.net_iface, None)?
            .into_mio_stream()
            .into_tls_websocket(self.url);

        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@ticker"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&self, ws: &mut TlsWebsocket<Self::Stream>, _ctx: &mut FeedContext) -> io::Result<()> {
        while let Some(WebsocketFrame::Text(ts, fin, data)) = ws.receive_next()? {
            info!("{ts}: ({fin}) {}", String::from_utf8_lossy(data));
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut context = FeedContext;

    let mut io_service =
        MioSelector::new()?.into_io_service_with_context(IdleStrategy::Sleep(Duration::from_millis(1)), &mut context);

    let ticker = MarketDataEndpoint::Ticker(TickerEndpoint::new(0, "wss://stream.binance.com:443/ws", None, "btcusdt"));
    let trade = MarketDataEndpoint::Trade(TradeEndpoint::new(1, "wss://stream.binance.com:443/ws", None, "ethusdt"));

    io_service.register(ticker);
    io_service.register(trade);

    loop {
        io_service.poll(&mut context)?;
    }
}
