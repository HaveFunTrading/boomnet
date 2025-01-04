use std::array::TryFromSliceError;
use std::io;
use std::io::ErrorKind::Other;
use thiserror::Error;
use url::ParseError;

#[derive(Error, Debug)]
pub enum Error {
    #[error("the peer has sent the close frame: status code {0}, body: {1}")]
    ReceivedCloseFrame(u16, String),
    #[error("websocket protocol error: {0}")]
    Protocol(&'static str),
    #[error("the websocket is closed and can be dropped")]
    Closed,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
    #[error("url parse error: {0}")]
    InvalidUrl(#[from] ParseError),
    #[error("slice error: {0}")]
    SliceError(#[from] TryFromSliceError),
}

impl From<Error> for io::Error {
    fn from(value: Error) -> Self {
        io::Error::new(Other, value)
    }
}
