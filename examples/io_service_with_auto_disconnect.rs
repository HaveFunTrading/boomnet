use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use idle::IdleStrategy;

use boomnet::endpoint::ws::{TlsWebsocket, TlsWebsocketEndpoint};
use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
use boomnet::select::mio::MioSelector;
use boomnet::service::IntoIOService;
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::BindAndConnect;
use boomnet::ws::{IntoTlsWebsocket, WebsocketFrame};

struct TradeEndpoint {
    id: u32,
    url: String,
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
            url: url.to_owned(),
            net_iface,
            instrument,
        }
    }
}

impl TlsWebsocketEndpoint for TradeEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        self.url.as_str()
    }

    fn create_websocket(&mut self, addr: SocketAddr) -> io::Result<TlsWebsocket<Self::Stream>> {
        let mut ws = TcpStream::bind_and_connect(addr, self.net_iface, None)?
            .into_mio_stream()
            .into_tls_websocket(self.url.as_str());

        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>) -> io::Result<()> {
        while let Some(WebsocketFrame::Text(ts, fin, data)) = ws.receive_next()? {
            println!("[{}] {ts}: ({fin}) {}", self.id, String::from_utf8_lossy(data));
        }
        Ok(())
    }

    fn can_recreate(&mut self) -> bool {
        // this method is called by IO service upon every disconnect
        // we can tap into it to perform any cleanup logic if required
        println!("handling can_recreate");
        true
    }

    fn can_auto_disconnect(&mut self) -> bool {
        // this method is called by IO service if auto_disconnect is enabled just before
        // disconnecting the endpoint
        println!("handling can_auto_disconnect");
        true
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?
        .into_io_service(IdleStrategy::Sleep(Duration::from_millis(1)))
        .with_auto_disconnect(Duration::from_secs(10));

    let endpoint_btc = TradeEndpoint::new(0, "wss://stream1.binance.com:443/ws", None, "btcusdt");

    io_service.register(endpoint_btc);

    loop {
        io_service.poll()?;
    }
}
