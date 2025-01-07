#![allow(unused)]

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
use boomnet::service::endpoint::ws::{TlsWebsocket, TlsWebsocketEndpoint, TlsWebsocketEndpointWithContext};
use boomnet::service::endpoint::Context;
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IntoIOService, IntoIOServiceWithContext};
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::tls::TlsStream;
use boomnet::stream::{BindAndConnect, ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoTlsWebsocket, Websocket, WebsocketFrame};
use idle::IdleStrategy;
use log::info;
use url::Url;

enum MarketDataEndpoint {
    Trade(TradeEndpoint),
    Ticker(TickerEndpoint),
}

impl ConnectionInfoProvider for MarketDataEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        match self {
            MarketDataEndpoint::Ticker(ticker) => ticker.connection_info(),
            MarketDataEndpoint::Trade(trade) => trade.connection_info(),
        }
    }
}

impl TlsWebsocketEndpoint for MarketDataEndpoint {
    type Stream = MioStream;

    fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<Websocket<TlsStream<Self::Stream>>> {
        match self {
            MarketDataEndpoint::Ticker(ticker) => ticker.create_websocket(addr),
            MarketDataEndpoint::Trade(trade) => trade.create_websocket(addr),
        }
    }

    fn poll(&mut self, ws: &mut Websocket<TlsStream<Self::Stream>>) -> io::Result<()> {
        match self {
            MarketDataEndpoint::Ticker(ticker) => TlsWebsocketEndpoint::poll(ticker, ws),
            MarketDataEndpoint::Trade(trade) => TlsWebsocketEndpoint::poll(trade, ws),
        }
    }
}

struct TradeEndpoint {
    id: u32,
    connection_info: ConnectionInfo,
    instrument: &'static str,
}

impl TradeEndpoint {
    pub fn new(id: u32, url: &'static str, instrument: &'static str) -> TradeEndpoint {
        let connection_info = Url::parse(url).try_into().unwrap();
        Self {
            id,
            connection_info,
            instrument,
        }
    }
}

impl ConnectionInfoProvider for TradeEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl TlsWebsocketEndpoint for TradeEndpoint {
    type Stream = MioStream;

    fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<TlsWebsocket<Self::Stream>> {
        let mut ws = self
            .connection_info
            .clone()
            .into_tcp_stream_with_addr(addr)?
            .into_mio_stream()
            .into_tls_websocket("/ws");

        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>) -> io::Result<()> {
        while let Some(WebsocketFrame::Text(fin, data)) = ws.receive_next()? {
            info!("({fin}) {}", String::from_utf8_lossy(data));
        }
        Ok(())
    }
}

struct TickerEndpoint {
    id: u32,
    connection_info: ConnectionInfo,
    instrument: &'static str,
}

impl TickerEndpoint {
    pub fn new(id: u32, url: &'static str, instrument: &'static str) -> TickerEndpoint {
        let connection_info = Url::parse(url).try_into().unwrap();
        Self {
            id,
            connection_info,
            instrument,
        }
    }
}

impl ConnectionInfoProvider for TickerEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl TlsWebsocketEndpoint for TickerEndpoint {
    type Stream = MioStream;

    fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<TlsWebsocket<Self::Stream>> {
        let mut ws = self
            .connection_info
            .clone()
            .into_tcp_stream_with_addr(addr)?
            .into_mio_stream()
            .into_tls_websocket("/ws");

        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@ticker"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>) -> io::Result<()> {
        #[allow(deprecated)]
        while let Some(WebsocketFrame::Text(fin, data)) = ws.receive_next()? {
            info!("({fin}) {}", String::from_utf8_lossy(data));
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?.into_io_service();

    let ticker = MarketDataEndpoint::Ticker(TickerEndpoint::new(0, "wss://stream.binance.com:443/ws", "btcusdt"));
    let trade = MarketDataEndpoint::Trade(TradeEndpoint::new(1, "wss://stream.binance.com:443/ws", "ethusdt"));

    io_service.register(ticker);
    io_service.register(trade);

    loop {
        io_service.poll()?;
    }
}
