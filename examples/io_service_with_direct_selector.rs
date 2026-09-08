use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
use boomnet::service::endpoint::EndpointFactory;
use boomnet::service::select::direct::DirectSelector;
use boomnet::service::{IOServiceEvent, IntoIOService};
use boomnet::stream::tls::TlsStream;
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider, tcp};
use boomnet::ws::{IntoTlsWebsocket, Websocket, WebsocketFrame};
use std::io;
use std::net::SocketAddr;
use url::Url;

struct TradeEndpointFactory {
    connection_info: ConnectionInfo,
    instrument: &'static str,
    ws_endpoint: String,
}

impl TradeEndpointFactory {
    pub fn new(
        _id: u32,
        url: &'static str,
        net_iface: Option<&'static str>,
        instrument: &'static str,
    ) -> TradeEndpointFactory {
        let url = Url::parse(url).unwrap();
        let mut connection_info = ConnectionInfo::try_from(url.clone()).unwrap();
        let ws_endpoint = url.path().to_owned();
        let net_iface = net_iface
            .and_then(|name| name.into_network_interface())
            .and_then(|iface| iface.to_socket_addr());
        if let Some(net_iface) = net_iface {
            connection_info = connection_info.with_net_iface(net_iface);
        }
        Self {
            connection_info,
            instrument,
            ws_endpoint,
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
    type Endpoint = Websocket<TlsStream<tcp::TcpStream>>;

    fn create_endpoint(&mut self, addr: SocketAddr, _ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {
        let mut ws = self
            .connection_info
            .clone()
            .into_tcp_stream_with_addr(addr)?
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

    let mut io_service = DirectSelector::new()?.into_io_service();

    let factory_btc = TradeEndpointFactory::new(0, "wss://stream1.binance.com:443/ws", None, "btcusdt");
    let factory_eth = TradeEndpointFactory::new(1, "wss://stream2.binance.com:443/ws", None, "ethusdt");
    let factory_xrp = TradeEndpointFactory::new(2, "wss://stream3.binance.com:443/ws", None, "xrpusdt");

    io_service.register(factory_btc)?;
    io_service.register(factory_eth)?;
    io_service.register(factory_xrp)?;

    loop {
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                let handle = active.handle();
                let batch = active.try_with(|ws| {
                    ws.read_batch()
                        .map(|batch| batch.into_iter().map(|frame| frame.map_err(io::Error::from)))
                        .map_err(io::Error::from)
                })?;
                for frame in batch {
                    if let WebsocketFrame::Text(fin, data) = frame? {
                        println!("[{handle:?}] ({fin}) {}", String::from_utf8_lossy(data));
                    }
                }
            }
        }
    }
}
