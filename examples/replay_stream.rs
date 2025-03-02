use std::io::ErrorKind::UnexpectedEof;

use boomnet::stream::replay::ReplayStream;
use boomnet::ws::{Error, IntoWebsocket, WebsocketFrame};

fn main() -> anyhow::Result<()> {
    let mut ws = ReplayStream::from_file("plain_inbound")?.into_websocket("/ws");

    fn run<F: FnOnce() -> Result<(), Error>>(f: F) -> anyhow::Result<()> {
        match f() {
            Err(Error::IO(io_error)) if io_error.kind() == UnexpectedEof => Ok(()),
            Err(err) => Err(err)?,
            _ => Ok(()),
        }
    }

    run(|| loop {
        for frame in ws.read_batch()? {
            if let WebsocketFrame::Text(fin, body) = frame? {
                println!("({fin}) {}", String::from_utf8_lossy(body));
            }
        }
    })?;

    Ok(())
}
