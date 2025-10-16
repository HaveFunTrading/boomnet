use boomnet::service::endpoint::{Context, EndpointWithContext};
use boomnet::stream::buffer::{BufferedStream, IntoBufferedStream};
use boomnet::stream::tcp::TcpStream;
use boomnet::stream::{ConnectionInfo, ConnectionInfoProvider};
use boomnet::ws::{IntoWebsocket, Websocket};
use std::hint::black_box;
use std::net::SocketAddr;

pub struct TestContext {
    pub wants_write: bool,
    pub processed: usize,
}

impl Context for TestContext {}

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
    payload: &'static str,
}

impl ConnectionInfoProvider for TestEndpoint {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl EndpointWithContext<TestContext> for TestEndpoint {
    type Target = Websocket<BufferedStream<TcpStream>>;

    fn create_target(&mut self, addr: SocketAddr, _ctx: &mut TestContext) -> std::io::Result<Option<Self::Target>> {
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
    pub fn new(port: u16, payload: &'static str) -> Self {
        Self {
            connection_info: ConnectionInfo::new("127.0.0.1", port),
            payload,
        }
    }

    pub fn poll(
        &mut self,
        ws: &mut <Self as EndpointWithContext<TestContext>>::Target,
        ctx: &mut TestContext,
    ) -> std::io::Result<()> {
        if ctx.wants_write {
            ws.send_text(true, Some(self.payload.as_bytes()))?;
            ctx.wants_write = false;
        } else {
            for frame in ws.read_batch()? {
                black_box(frame?);
                ctx.processed += 1;
            }
        }
        Ok(())
    }
}
