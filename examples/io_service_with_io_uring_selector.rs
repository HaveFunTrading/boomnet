use boomnet::service::endpoint::EndpointFactory;
use boomnet::service::select::io_uring::{IoUringConfig, IoUringSelector};
use boomnet::service::{IOServiceEvent, IntoIOService};
use boomnet::stream::io_uring::{IntoIoUringStream, IoUringStream};
use boomnet::stream::tls::TlsStream;
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoTlsWebsocket, Websocket, WebsocketFrame};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

struct TradeEndpointFactory {
    connection_info: ConnectionInfo,
    instrument: &'static str,
    ws_endpoint: String,
}

impl TradeEndpointFactory {
    fn new(url: &'static str, instrument: &'static str) -> Self {
        let url = Url::parse(url).unwrap();
        Self {
            connection_info: ConnectionInfo::try_from(url.clone()).unwrap(),
            instrument,
            ws_endpoint: url.path().to_owned(),
        }
    }
}

impl ConnectionInfoProvider for TradeEndpointFactory {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl EndpointFactory for TradeEndpointFactory {
    type Context = ();
    type Endpoint = Websocket<TlsStream<IoUringStream>>;

    fn create_endpoint(&mut self, addr: SocketAddr, _ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {
        let mut ws = self
            .connection_info
            .clone()
            .into_tcp_stream_with_addr(addr)?
            .into_io_uring_stream()
            .into_tls_websocket(&self.ws_endpoint)?;
        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;
        Ok(Some(ws))
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Ring-level preferred busy polling requires CAP_NET_ADMIN and Linux 6.9 or newer.
    // The bounded wait drives NAPI from this thread but never waits longer than 50 microseconds.
    let selector = IoUringSelector::new_with_config(IoUringConfig {
        entries: 64,
        wait_timeout: Some(Duration::from_micros(50)),
        napi_busy_poll_timeout: Some(50),
        prefer_busy_poll: false,
    })?;
    let mut io_service = selector.into_io_service();

    io_service.register(TradeEndpointFactory::new("wss://stream.binance.com:443/ws", "btcusdt"))?;
    io_service.register(TradeEndpointFactory::new("wss://stream.binance.com:443/ws", "ethusdt"))?;

    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                let Some(batch) = active.try_with(|ws| {
                    ws.read_batch()
                        .map(|batch| batch.into_iter().map(|frame| frame.map_err(io::Error::from)))
                        .map_err(io::Error::from)
                }) else {
                    continue;
                };
                for frame in batch {
                    if let WebsocketFrame::Text(fin, data) = frame {
                        println!("({fin}) {}", String::from_utf8_lossy(data));
                    }
                }
            }
        }
    }
}
