use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
use boomnet::service::endpoint::ws::{TlsWebsocket, TlsWebsocketEndpoint};
use boomnet::service::select::mio::MioSelector;
use boomnet::service::IntoIOService;
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoTlsWebsocket, WebsocketFrame};
use std::io;
use std::net::SocketAddr;
use url::Url;

struct TradeEndpoint {
    id: u32,
    connection_info: ConnectionInfo,
    net_iface: Option<SocketAddr>,
    instrument: &'static str,
}

impl TradeEndpoint {
    pub fn new(id: u32, url: &'static str, net_iface: Option<&'static str>, instrument: &'static str) -> TradeEndpoint {
        let connection_info = Url::parse(url).try_into().unwrap();
        let net_iface = net_iface
            .and_then(|name| name.into_network_interface())
            .and_then(|iface| iface.to_socket_addr());
        Self {
            id,
            connection_info,
            net_iface,
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

        // let mut ws = TcpStream::bind_and_connect(addr, self.net_iface, None)?
        //     .into_mio_stream()
        //     .into_tls_websocket(self.url);
        //
        // ws.send_text(
        //     true,
        //     Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        // )?;
        //
        // Ok(ws)
    }

    #[inline]
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>) -> io::Result<()> {
        for frame in ws.batch_iter()? {
            if let WebsocketFrame::Text(fin, data) = frame? {
                println!("[{}] ({fin}) {}", self.id, String::from_utf8_lossy(data));
            }
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?.into_io_service();

    let endpoint_btc = TradeEndpoint::new(0, "wss://stream1.binance.com:443/ws", None, "btcusdt");
    let endpoint_eth = TradeEndpoint::new(1, "wss://stream2.binance.com:443/ws", None, "ethusdt");
    let endpoint_xrp = TradeEndpoint::new(2, "wss://stream3.binance.com:443/ws", None, "xrpusdt");

    io_service.register(endpoint_btc);
    io_service.register(endpoint_eth);
    io_service.register(endpoint_xrp);

    loop {
        io_service.poll()?;
    }
}
