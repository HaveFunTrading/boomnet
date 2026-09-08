//! The timer belongs to application processing; endpoint lifecycle callbacks need no context.

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use boomnet::service::endpoint::EndpointFactory;
use boomnet::service::select::mio::MioSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::tls::TlsStream;
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoTlsWebsocket, Websocket, WebsocketFrame};
use log::info;
use url::Url;

/// This example demonstrates how application logic can request a disconnect through an active
/// endpoint guard. In this case, the endpoint is recreated every 10 seconds.
struct TradeEndpointFactory {
    connection_info: ConnectionInfo,
    instrument: &'static str,
}

impl TradeEndpointFactory {
    pub fn new(url: &'static str, instrument: &'static str) -> TradeEndpointFactory {
        let connection_info = Url::parse(url).try_into().unwrap();
        Self {
            connection_info,
            instrument,
        }
    }
}

#[derive(Debug)]
struct FeedContext {
    next_disconnect_time_ns: u64,
}

impl FeedContext {
    pub fn new() -> Self {
        let mut context = Self {
            next_disconnect_time_ns: 0,
        };
        context.next_disconnect_time_ns = context.current_time_ns() + Duration::from_secs(10).as_nanos() as u64;
        context
    }

    pub fn current_time_ns(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    fn should_disconnect(&mut self) -> bool {
        let now_ns = self.current_time_ns();
        if now_ns <= self.next_disconnect_time_ns {
            return false;
        }
        self.next_disconnect_time_ns = now_ns + Duration::from_secs(10).as_nanos() as u64;
        true
    }
}

impl ConnectionInfoProvider for TradeEndpointFactory {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl EndpointFactory for TradeEndpointFactory {
    type Context = ();
    type Endpoint = Websocket<TlsStream<MioStream>>;

    fn create_endpoint(&mut self, addr: SocketAddr, _ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {
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

        Ok(Some(ws))
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut ctx = FeedContext::new();

    let mut io_service = MioSelector::new()?.into_io_service();

    let factory_btc = TradeEndpointFactory::new("wss://stream1.binance.com:443/ws", "btcusdt");

    io_service.register(factory_btc)?;
    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                if ctx.should_disconnect() {
                    let _ = active.try_with::<()>(|_| Err(io::Error::other("timer expired")));
                    continue;
                }
                let batch = active.try_with(|ws| {
                    ws.read_batch()
                        .map(|batch| batch.into_iter().map(|frame| frame.map_err(io::Error::from)))
                        .map_err(io::Error::from)
                })?;
                for frame in batch {
                    if let WebsocketFrame::Text(fin, data) = frame? {
                        info!("({fin}) {}", String::from_utf8_lossy(data));
                    }
                }
            }
        }
    }
}
