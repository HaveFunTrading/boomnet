use crate::ws::decoder::Decoder;
use crate::ws::handshake::Handshaker;
use crate::ws::{Error, Websocket, WebsocketFrame};
use std::collections::VecDeque;
use std::io;

pub trait DataSource {
    fn next(&self) -> Result<Option<WebsocketFrame>, Error>;

    fn into_stream(self) -> DataSourceStream<Self>
    where
        Self: Sized,
    {
        DataSourceStream { data_source: self }
    }
}

pub struct DataSourceStream<D> {
    data_source: D,
}

impl<D: DataSource> Websocket<DataSourceStream<D>> {
    pub fn receive_next(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        self.stream.data_source.next()
    }
}

impl<D: DataSource> Websocket<D> {
    pub fn from_data_source(data_source: D) -> io::Result<Websocket<DataSourceStream<D>>> {
        Ok(Websocket {
            stream: data_source.into_stream(),
            handshaker: Handshaker::new(),
            frame: Decoder::new(),
            closed: false,
            pending_pong: false,
            pong_payload: Vec::with_capacity(4096),
            pending_msg_buffer: VecDeque::with_capacity(256),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_custom_data_source() {
        struct CustomDataSource;

        impl DataSource for CustomDataSource {
            fn next(&self) -> Result<Option<WebsocketFrame>, Error> {
                Ok(Some(WebsocketFrame::Text(1, true, b"foo")))
            }
        }

        let mut ws = Websocket::from_data_source(CustomDataSource).unwrap();

        if let Some(WebsocketFrame::Text(_ts, _fin, data)) = ws.receive_next().unwrap() {
            assert_eq!(b"foo", data)
        } else {
            panic!("test failed")
        }
    }
}
