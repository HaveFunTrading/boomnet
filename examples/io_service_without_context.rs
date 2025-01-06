use std::io;
use std::net::{SocketAddr, TcpStream};

use boomnet::endpoint::ws::{TlsWebsocket, TlsWebsocketEndpoint};
use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
use boomnet::select::mio::MioSelector;
use boomnet::service::IntoIOService;
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::BindAndConnect;
use boomnet::ws::{WebsocketFrame};

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

impl TlsWebsocketEndpoint for TradeEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        self.url
    }

    fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<TlsWebsocket<Self::Stream>> {
        todo!()
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
