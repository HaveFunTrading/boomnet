use boomnet::service::endpoint::Endpoint;
use boomnet::stream::buffer::{BufferedStream, IntoBufferedStream};
use boomnet::stream::tcp::TcpStream;
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoWebsocket, Websocket};
use std::net::SocketAddr;

pub struct TestContext {
    pub wants_write: bool,
    pub processed: usize,
}

impl TestContext {
    pub fn new() -> TestContext {
        Self {
            wants_write: true,
            processed: 0,
        }
    }
}

pub struct TestEndpoint {
    connection_info: ConnectionInfo,
}

impl ConnectionInfoProvider for TestEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl Endpoint for TestEndpoint {
    type Context = ();
    type Target = Websocket<BufferedStream<TcpStream>>;

    fn create_target(&mut self, addr: SocketAddr, _ctx: &mut Self::Context) -> std::io::Result<Option<Self::Target>> {
        let ws = self
            .connection_info
            .clone()
            .into_tcp_stream_with_addr(addr)?
            .into_default_buffered_stream()
            .into_websocket("/");
        Ok(Some(ws))
    }
}

impl TestEndpoint {
    pub fn new(port: u16, _payload: &'static str) -> Self {
        Self {
            connection_info: ConnectionInfo::new("127.0.0.1", port),
        }
    }
}
