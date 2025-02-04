use std::io;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use boomnet::service::endpoint::ws::{TlsWebsocket, TlsWebsocketEndpointWithContext};
use boomnet::service::endpoint::Context;
use boomnet::service::select::mio::MioSelector;
use boomnet::service::IntoIOServiceWithContext;
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoTlsWebsocket, WebsocketFrame};
use log::info;
use url::Url;

/// This example demonstrates how to implement explicit timer inside the endpoint. Since endpoint
/// poll method is called on every cycle by the io service we can implement timer functionality
/// directly inside the endpoint. In this case, the endpoint will keep disconnecting every 10s.
struct TradeEndpoint {
    connection_info: ConnectionInfo,
    instrument: &'static str,
    next_disconnect_time_ns: u64,
}

impl TradeEndpoint {
    pub fn new(url: &'static str, instrument: &'static str, ctx: &FeedContext) -> TradeEndpoint {
        let connection_info = Url::parse(url).try_into().unwrap();
        Self {
            connection_info,
            instrument,
            next_disconnect_time_ns: ctx.current_time_ns() + Duration::from_secs(10).as_nanos() as u64,
        }
    }
}

#[derive(Debug)]
struct FeedContext;

impl Context for FeedContext {}

impl FeedContext {
    pub fn new() -> Self {
        Self
    }

    pub fn current_time_ns(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }
}

impl ConnectionInfoProvider for TradeEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl TlsWebsocketEndpointWithContext<FeedContext> for TradeEndpoint {
    type Stream = MioStream;

    fn create_websocket(&mut self, addr: SocketAddr, _ctx: &mut FeedContext) -> io::Result<TlsWebsocket<Self::Stream>> {
        let mut ws = self
            .connection_info
            .clone()
            .into_tcp_stream_with_addr(addr)?
            .into_mio_stream()
            .into_tls_websocket("/ws")?;

        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>, ctx: &mut FeedContext) -> io::Result<()> {
        while let Some(Ok(WebsocketFrame::Text(fin, data))) = ws.receive_next() {
            info!("({fin}) {}", String::from_utf8_lossy(data));
        }
        let now_ns = ctx.current_time_ns();
        if now_ns > self.next_disconnect_time_ns {
            self.next_disconnect_time_ns = now_ns + Duration::from_secs(10).as_nanos() as u64;
            return Err(io::Error::other("disconnected due to timer"));
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut ctx = FeedContext::new();

    let mut io_service = MioSelector::new()?.into_io_service_with_context(&mut ctx);

    let endpoint_btc = TradeEndpoint::new("wss://stream1.binance.com:443/ws", "btcusdt", &ctx);

    io_service.register(endpoint_btc);

    loop {
        io_service.poll(&mut ctx)?;
    }
}
