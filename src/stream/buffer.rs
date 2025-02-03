//! Stream that is buffering data written to it.

use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use std::io;
use std::io::{ErrorKind, Read, Write};
use std::mem::MaybeUninit;

/// Default buffer size in bytes.
pub const DEFAULT_BUFFER_SIZE: usize = 1024;

/// Buffers data written to it until explicitly flushed. Useful if you
/// want to reduce the number of operating system calls when writing. If there
/// is no more space in the buffer to accommodate the current write it
/// will return [ErrorKind::WriteZero].
///
/// # Examples
///
/// Wrap with default BufferedStream`.
///
/// ``` no_run
/// use boomnet::stream::buffer::IntoBufferedStream;
/// use boomnet::stream::ConnectionInfo;
/// use boomnet::stream::tls::IntoTlsStream;
/// use boomnet::ws::IntoWebsocket;
///
/// let mut ws = ConnectionInfo::new("stream.binance.com", 9443)
///  .into_tcp_stream().unwrap()
///  .into_tls_stream().unwrap()
///  .into_default_buffered_stream()
///  .into_websocket("/ws");
/// ```
///
/// Specify buffer size when wrapping.
///
/// ``` no_run
/// use boomnet::stream::buffer::IntoBufferedStream;
/// use boomnet::stream::ConnectionInfo;
/// use boomnet::stream::tls::IntoTlsStream;
/// use boomnet::ws::IntoWebsocket;
///
/// let mut ws = ConnectionInfo::new("stream.binance.com", 9443)
///  .into_tcp_stream().unwrap()
///  .into_tls_stream().unwrap()
///  .into_buffered_stream::<512>()
///  .into_websocket("/ws");
/// ```
pub struct BufferedStream<S, const N: usize = DEFAULT_BUFFER_SIZE> {
    inner: S,
    buffer: [u8; N],
    cursor: usize,
}

impl<S: Read, const N: usize> Read for BufferedStream<S, N> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<S: Write, const N: usize> Write for BufferedStream<S, N> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cold]
        fn handle_overflow() -> io::Result<()> {
            Err(io::Error::new(ErrorKind::WriteZero, "unable to write the whole buffer"))
        }

        let len = buf.len();
        let remaining = N - self.cursor;
        if len > remaining {
            handle_overflow()?
        }
        self.buffer[self.cursor..self.cursor + len].copy_from_slice(buf);
        self.cursor += len;
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.write_all(&self.buffer[..self.cursor])?;
        self.cursor = 0;
        self.inner.flush()
    }
}

impl<S: ConnectionInfoProvider, const N: usize> ConnectionInfoProvider for BufferedStream<S, N> {
    fn connection_info(&self) -> &ConnectionInfo {
        self.inner.connection_info()
    }
}

/// Trait to convert any stream into `BufferedStream`.
pub trait IntoBufferedStream<S> {
    /// Convert into `BufferedStream` and specify buffer length.
    fn into_buffered_stream<const N: usize>(self) -> BufferedStream<S, N>;

    /// Convert into `BufferedStream` with default buffer length.
    fn into_default_buffered_stream(self) -> BufferedStream<S>
    where
        Self: Sized,
    {
        Self::into_buffered_stream(self)
    }
}

impl<T> IntoBufferedStream<T> for T
where
    T: Read + Write + ConnectionInfoProvider,
{
    fn into_buffered_stream<const N: usize>(self) -> BufferedStream<T, N> {
        unsafe {
            BufferedStream {
                inner: self,
                buffer: MaybeUninit::uninit().assume_init(),
                cursor: 0,
            }
        }
    }
}
