use boomnet::service::ActiveEndpoint;
use boomnet::service::endpoint::{DisconnectReason, EndpointFactory};
use boomnet::stream::mio::{IntoMioStream, MioStream};
use boomnet::stream::tcp::TcpStream;
use boomnet::stream::tls::{IntoTlsStream, TlsConfigExt, TlsStream};
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoWebsocket, Websocket, WebsocketFrame};
use log::{info, warn};
use std::io;
use std::net::SocketAddr;

pub struct TradeEndpointFactory {
    connection_info: ConnectionInfo,
    instrument: &'static str,
    ws_endpoint: String,
    subscribe: bool,
}

#[allow(dead_code)]
pub fn process_active(active: ActiveEndpoint<'_, Websocket<TlsStream<MioStream>>>) -> io::Result<()> {
    let handle = active.handle();
    let batch = active.try_with(|ws| {
        ws.read_batch()
            .map(|batch| batch.into_iter().map(|frame| frame.map_err(io::Error::from)))
            .map_err(io::Error::from)
    })?;
    for frame in batch {
        if let WebsocketFrame::Text(fin, data) = frame? {
            info!("[{handle:?}] ({fin}) {}", String::from_utf8_lossy(data));
        }
    }
    Ok(())
}

impl TradeEndpointFactory {
    #[allow(dead_code)]
    pub fn new(url: &'static str, net_iface: Option<&'static str>, instrument: &'static str) -> TradeEndpointFactory {
        Self::new_with_subscribe(url, net_iface, instrument, true)
    }

    pub fn new_with_subscribe(
        url: &'static str,
        net_iface: Option<&'static str>,
        instrument: &'static str,
        subscribe: bool,
    ) -> TradeEndpointFactory {
        let (mut connection_info, ws_endpoint, _) = boomnet::ws::util::parse_url(url).unwrap();
        if let Some(net_iface) = net_iface {
            connection_info = connection_info.with_net_iface_from_name(net_iface);
        }
        Self {
            connection_info,
            instrument,
            ws_endpoint,
            subscribe,
        }
    }

    pub fn subscribe(&mut self, ws: &mut Websocket<TlsStream<MioStream>>) -> io::Result<()> {
        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;
        Ok(())
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
        let mut ws = TcpStream::try_from((&self.connection_info, addr))?
            .into_mio_stream()
            .into_tls_stream_with_config(|cfg| cfg.with_no_cert_verification())?
            .into_websocket(&self.ws_endpoint);

        if self.subscribe {
            self.subscribe(&mut ws)?;
        }

        Ok(Some(ws))
    }
    fn can_recreate(&mut self, reason: &DisconnectReason, _ctx: &mut Self::Context) -> bool {
        warn!("connection disconnected: {reason}");
        true
    }
}
