use std::io;
use std::io::ErrorKind::{Other, WouldBlock};
use std::io::{Read, Write};

use base64::engine::general_purpose;
use base64::Engine;
use http::StatusCode;
use httparse::Response;
use rand::{thread_rng, Rng};
use url::Url;

use crate::buffer::ReadBuffer;
use crate::ws::handshake::HandshakeState::{Completed, NotStarted, Pending};
use crate::ws::Error;

#[derive(Debug)]
pub(crate) struct Handshaker {
    buffer: ReadBuffer<1>,
    state: HandshakeState,
    url: Url,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum HandshakeState {
    NotStarted,
    Pending,
    Completed,
}

impl Handshaker {
    pub(crate) fn new(url: &str) -> Result<Self, Error> {
        let url = Url::parse(url)?;
        Ok(Self {
            buffer: ReadBuffer::new(),
            state: NotStarted,
            url,
        })
    }

    pub(crate) fn completed() -> Result<Self, Error> {
        Ok(Self {
            buffer: ReadBuffer::new(),
            state: Completed,
            url: Url::parse("ws://localhost:3012")?, // does not matter
        })
    }

    pub(crate) const fn state(&self) -> HandshakeState {
        self.state
    }

    pub(crate) fn await_handshake<S: Read>(&mut self, stream: &mut S) -> io::Result<()> {
        match self.state {
            NotStarted => panic!("websocket handshake not started"),
            Pending => {
                self.buffer.read_from(stream)?;
                let available = self.buffer.available();
                if available >= 4 && self.buffer.view_last(4) == b"\r\n\r\n" {
                    // decode http response
                    let mut headers = [httparse::EMPTY_HEADER; 64];
                    let mut response = Response::new(&mut headers);
                    response
                        .parse(self.buffer.view())
                        .map_err(|err| io::Error::new(Other, err))?;
                    if response.code.unwrap() != StatusCode::SWITCHING_PROTOCOLS.as_u16() {
                        return Err(io::Error::new(Other, "unable to switch protocols"));
                    }
                    self.state = Completed;
                    return Ok(());
                }
                Err(io::Error::from(WouldBlock))
            }
            Completed => Ok(()),
        }
    }

    pub(crate) fn send_handshake_request<S: Write>(&mut self, stream: &mut S) -> io::Result<()> {
        stream.write_all(format!("GET {} HTTP/1.1\r\n", self.url.path()).as_bytes())?;
        stream.write_all(format!("Host: {}\r\n", self.url.host_str().unwrap()).as_bytes())?;
        stream.write_all(b"Upgrade: websocket\r\n")?;
        stream.write_all(b"Connection: upgrade\r\n")?;
        stream.write_all(format!("Sec-WebSocket-Key: {}\r\n", generate_nonce()).as_bytes())?;
        stream.write_all(b"Sec-WebSocket-Version: 13\r\n")?;
        stream.write_all(b"\r\n")?;
        stream.flush()?;
        self.state = Pending;
        Ok(())
    }
}

fn generate_nonce() -> String {
    let mut rng = thread_rng();
    let nonce_bytes: [u8; 16] = rng.gen();
    general_purpose::STANDARD.encode(nonce_bytes)
}
