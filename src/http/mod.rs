//! This module provides a reusable HTTP1.1 client built on top of a generic `ConnectionPool` trait.
//!
//! # Examples
//!
//! ```no_run
//! // Create a TLS connection pool
//! use http::Method;
//! use boomnet::http::{ConnectionPool, HttpClient, SingleTlsConnectionPool};
//! use boomnet::stream::ConnectionInfo;
//!
//! let mut client = SingleTlsConnectionPool::new(ConnectionInfo::new("example.com", 443)).into_http_client();
//!
//! // Send a GET request and block until complete
//! let (status, headers, body) = client
//!     .new_request(Method::GET, "/", None)
//!     .unwrap()
//!     .block()
//!     .unwrap();
//!
//! println!("Status: {}", status);
//! println!("Headers: {}", headers);
//! println!("Body: {}", body);
//! ```

use crate::stream::ConnectionInfo;
use crate::stream::buffer::{BufferedStream, IntoBufferedStream};
use crate::stream::tcp::TcpStream;
use crate::stream::tls::{IntoTlsStream, TlsConfigExt, TlsStream};
use crate::util::NoBlock;

use httparse::{EMPTY_HEADER, Response};
use memchr::arch::all::rabinkarp::Finder;
use std::cell::RefCell;
use std::io;
use std::io::{ErrorKind, Read, Write};
use std::ops::{Index, IndexMut};
use std::rc::Rc;

// re-export
pub use http::Method;
use smallvec::SmallVec;

/// Default capacity of the buffer when reading chunks of bytes from the stream.
pub const DEFAULT_CHUNK_SIZE: usize = 1024;

type HttpTlsConnection = Connection<BufferedStream<TlsStream<TcpStream>>>;

/// Re-usable container to store headers
#[derive(Default)]
pub struct Headers<'a> {
    inner: SmallVec<[(&'a str, &'a str); 32]>,
}

impl<'a> Index<&'a str> for Headers<'a> {
    type Output = &'a str;

    // Look up the first matching header
    // panics if not found
    fn index(&self, key: &'a str) -> &Self::Output {
        for pair in &self.inner {
            if pair.0 == key {
                return &pair.1;
            }
        }
        panic!("no header named `{key}`");
    }
}

impl<'a> IndexMut<&'a str> for Headers<'a> {
    fn index_mut(&mut self, key: &'a str) -> &mut Self::Output {
        // we push (key, "") and then hand back a &mut to the `&'a str` slot
        self.inner.push((key, ""));
        &mut self.inner.last_mut().unwrap().1
    }
}

impl<'a> Headers<'a> {
    /// Append key-value header to the outgoing request.
    #[inline]
    pub fn insert(&mut self, key: &'a str, value: &'a str) {
        self.inner.push((key, value));
    }

    /// Returns `true` if no headers are present.
    #[inline]
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns iterator over the headers.
    #[inline]
    fn iter(&self) -> impl Iterator<Item = &(&str, &str)> {
        self.inner.iter()
    }

    /// Clear all headers.
    #[inline]
    fn clear(&mut self) -> &mut Self {
        self.inner.clear();
        self
    }
}

/// A generic HTTP client that uses a pooled connection strategy.
pub struct HttpClient<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize = DEFAULT_CHUNK_SIZE> {
    connection_pool: Rc<RefCell<C>>,
    headers: Headers<'static>,
}

impl<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize> HttpClient<C, CHUNK_SIZE> {
    /// Create a new HTTP client from the provided pool.
    pub fn new(connection_pool: C) -> HttpClient<C, CHUNK_SIZE> {
        Self {
            connection_pool: Rc::new(RefCell::new(connection_pool)),
            headers: Headers {
                inner: SmallVec::with_capacity(32),
            },
        }
    }

    /// Prepare a request with custom headers and optional body.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use http::Method;
    /// use boomnet::http::{ConnectionPool, HttpClient, SingleTlsConnectionPool};
    /// use boomnet::stream::ConnectionInfo;
    ///
    /// let mut client = SingleTlsConnectionPool::new(ConnectionInfo::new("example.com", 443)).into_http_client();
    ///
    /// let req = client.new_request_with_headers(
    ///     Method::POST,
    ///     "/submit",
    ///     Some(b"data"),
    ///     |hdrs| {
    ///         hdrs["X-Custom"] = "Value";
    ///     }
    /// ).unwrap();
    /// ```
    pub fn new_request_with_headers<F>(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
        builder: F,
    ) -> io::Result<HttpRequest<C, CHUNK_SIZE>>
    where
        F: FnOnce(&mut Headers),
    {
        builder(self.headers.clear());
        let conn = self
            .connection_pool
            .borrow_mut()
            .acquire()?
            .ok_or_else(|| io::Error::other("no available connection"))?;
        let request = HttpRequest::new(method, path, body, &self.headers, conn, self.connection_pool.clone())?;
        Ok(request)
    }

    /// Prepare a request with no additional headers and optional body.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use http::Method;
    /// use boomnet::http::{ConnectionPool , SingleTlsConnectionPool};
    /// use boomnet::stream::ConnectionInfo;
    ///
    /// let mut client = SingleTlsConnectionPool::new(ConnectionInfo::new("example.com", 443)).into_http_client();
    /// let req = client.new_request(
    ///     Method::POST,
    ///     "/submit",
    ///     Some(b"data"),
    /// ).unwrap();
    /// ```
    pub fn new_request(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
    ) -> io::Result<HttpRequest<C, CHUNK_SIZE>> {
        self.new_request_with_headers(method, path, body, |_| {})
    }
}

/// Trait defining a pool of reusable connections.
pub trait ConnectionPool<const CHUNK_SIZE: usize = DEFAULT_CHUNK_SIZE>: Sized {
    /// Underlying stream type.
    type Stream: Read + Write;

    /// Turn this connection pool into http client.
    fn into_http_client(self) -> HttpClient<Self, CHUNK_SIZE>
    where
        Self: ConnectionPool<CHUNK_SIZE>,
    {
        HttpClient::new(self)
    }

    /// Hostname for requests.
    fn host(&self) -> &str;

    /// Acquire next free connection, if available.
    fn acquire(&mut self) -> io::Result<Option<Connection<Self::Stream, CHUNK_SIZE>>>;

    /// Release a connection back into the pool.
    fn release(&mut self, stream: Option<Connection<Self::Stream, CHUNK_SIZE>>);
}

/// A single-connection pool over TLS, reconnecting on demand.
pub struct SingleTlsConnectionPool {
    connection_info: ConnectionInfo,
    conn: Option<HttpTlsConnection>,
    has_active_connection: bool,
}

impl SingleTlsConnectionPool {
    /// Build a new TLS pool for the given connection info.
    pub fn new(connection_info: impl Into<ConnectionInfo>) -> SingleTlsConnectionPool {
        Self {
            connection_info: connection_info.into(),
            conn: None,
            has_active_connection: false,
        }
    }
}

impl ConnectionPool for SingleTlsConnectionPool {
    type Stream = BufferedStream<TlsStream<TcpStream>>;

    fn host(&self) -> &str {
        self.connection_info.host()
    }

    fn acquire(&mut self) -> io::Result<Option<Connection<Self::Stream>>> {
        match (self.conn.take(), self.has_active_connection) {
            (Some(_), true) => {
                // we can at most have one active connection
                unreachable!()
            }
            (Some(stream), false) => {
                self.has_active_connection = true;
                Ok(Some(stream))
            }
            (None, true) => Ok(None),
            (None, false) => {
                let stream = self
                    .connection_info
                    .clone()
                    .into_tcp_stream()?
                    .into_tls_stream_with_config(|tls_cfg| tls_cfg.with_no_cert_verification())?
                    .into_default_buffered_stream();
                self.has_active_connection = true;
                Ok(Some(Connection::new(stream)))
            }
        }
    }

    fn release(&mut self, conn: Option<Connection<Self::Stream>>) {
        self.has_active_connection = false;
        if let Some(conn) = conn {
            if !conn.disconnected {
                let _ = self.conn.insert(conn);
            }
        }
    }
}

/// Represents an in-flight HTTP exchange.
pub struct HttpRequest<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize = DEFAULT_CHUNK_SIZE> {
    conn: Option<Connection<C::Stream, CHUNK_SIZE>>,
    pool: Rc<RefCell<C>>,
    state: State,
}

#[derive(Debug, Eq, PartialEq)]
enum State {
    ReadingHeaders,
    ReadingBody {
        header_len: usize,
        content_len: usize,
        status_code: u16,
    },
    Done {
        header_len: usize,
        status_code: u16,
    },
}

impl<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize> HttpRequest<C, CHUNK_SIZE> {
    fn new(
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
        headers: &Headers,
        mut conn: Connection<C::Stream, CHUNK_SIZE>,
        pool: Rc<RefCell<C>>,
    ) -> io::Result<HttpRequest<C, CHUNK_SIZE>> {
        conn.write_all(method.as_str().as_bytes())?;
        conn.write_all(b" ")?;
        conn.write_all(path.as_ref().as_bytes())?;
        conn.write_all(b" HTTP/1.1\r\nHost: ")?;
        conn.write_all(pool.borrow().host().as_bytes())?;
        if !headers.is_empty() {
            conn.write_all(b"\r\n")?;
            for header in headers.iter() {
                conn.write_all(header.0.as_bytes())?;
                conn.write_all(b": ")?;
                conn.write_all(header.1.as_bytes())?;
                conn.write_all(b"\r\n")?;
            }
            if let Some(body) = body {
                conn.write_all(b"Content-Length: ")?;
                let mut buf = itoa::Buffer::new();
                conn.write_all(buf.format(body.len()).as_bytes())?;
                conn.write_all(b"\r\n")?;
            }
            conn.write_all(b"\r\n")?;
        } else if let Some(body) = body {
            conn.write_all(b"\r\n")?;
            conn.write_all(b"Content-Length: ")?;
            let mut buf = itoa::Buffer::new();
            conn.write_all(buf.format(body.as_ref().len()).as_bytes())?;
            conn.write_all(b"\r\n\r\n")?;
        } else {
            conn.write_all(b"\r\n\r\n")?;
        }
        if let Some(body) = body {
            conn.write_all(body)?;
        }
        conn.flush()?;
        Ok(Self {
            conn: Some(conn),
            pool,
            state: State::ReadingHeaders,
        })
    }

    /// Block until the full response is available.
    #[inline]
    pub fn block(mut self) -> io::Result<(u16, String, String)> {
        loop {
            if let Some((status_code, headers, body)) = self.poll()? {
                return Ok((status_code, headers.to_owned(), body.to_owned()));
            }
        }
    }

    /// Read from the stream and return when complete. Must provide buffer that will hold the response.
    /// It's ok to re-use the buffer as long as it's been cleared before using it with a new request.
    ///
    /// # Example
    /// ```no_run
    /// use http::Method;
    /// use boomnet::http::{ConnectionPool , SingleTlsConnectionPool};
    /// use boomnet::stream::ConnectionInfo;
    ///
    /// let mut client = SingleTlsConnectionPool::new(ConnectionInfo::new("example.com", 443)).into_http_client();
    ///
    /// let mut request = client.new_request_with_headers(
    ///     Method::POST,
    ///     "/submit",
    ///     Some(b"data"),
    ///     |hdrs| {
    ///         hdrs["X-Custom"] = "Value";
    ///     }
    /// ).unwrap();
    ///
    /// loop {
    ///     if let Some((status_code, headers, body)) = request.poll().unwrap() {
    ///         println!("{}", status_code);
    ///         println!("{}", headers);
    ///         println!("{}", body);
    ///         break;
    ///     }
    /// }
    ///
    /// ```
    pub fn poll(&mut self) -> io::Result<Option<(u16, &str, &str)>> {
        if let Some(conn) = self.conn.as_mut() {
            match self.state {
                State::ReadingHeaders | State::ReadingBody { .. } => conn.poll()?,
                State::Done { .. } => {}
            }
            match self.state {
                State::ReadingHeaders => {
                    if conn.buffer.len() >= 4 {
                        if let Some(headers_end) = conn.header_finder.find(&conn.buffer, b"\r\n\r\n") {
                            let header_len = headers_end + 4;
                            let header_slice = &conn.buffer[..header_len];
                            // now parse headers
                            let mut headers = [EMPTY_HEADER; 32];
                            let mut resp = Response::new(&mut headers);
                            match resp.parse(header_slice) {
                                Ok(httparse::Status::Complete(_)) => {
                                    let status_code = resp
                                        .code
                                        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "missing status code"))?;
                                    let mut content_len = 0;
                                    for header in resp.headers {
                                        if header.name.eq_ignore_ascii_case("Content-Length") {
                                            content_len = std::str::from_utf8(header.value)
                                                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?
                                                .parse()
                                                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                                            break;
                                        }
                                    }
                                    self.state = State::ReadingBody {
                                        header_len,
                                        content_len,
                                        status_code,
                                    };
                                }
                                Ok(httparse::Status::Partial) => {
                                    return Err(io::Error::new(ErrorKind::InvalidData, "unable to parse headers"));
                                }
                                Err(err) => return Err(io::Error::new(ErrorKind::InvalidData, err)),
                            }
                        }
                    }
                }
                State::ReadingBody {
                    header_len,
                    content_len,
                    status_code,
                } => {
                    let total_len = header_len + content_len;
                    if conn.buffer.len() >= total_len {
                        self.state = State::Done {
                            header_len,
                            status_code,
                        };
                    }
                }
                State::Done {
                    header_len,
                    status_code,
                } => {
                    let (headers, body) = conn.buffer.split_at(header_len);
                    let headers =
                        std::str::from_utf8(headers).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                    let body = std::str::from_utf8(body).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                    return Ok(Some((status_code, headers, body)));
                }
            }
        }
        Ok(None)
    }
}

impl<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize> Drop for HttpRequest<C, CHUNK_SIZE> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.as_mut() {
            conn.buffer.clear();
        }
        self.pool.borrow_mut().release(self.conn.take());
    }
}

/// Connection managed by the `ConnectionPool`. Binds underlying stream together with buffer used
/// for reading data. The reading is performed in chunks with default size of 1024 bytes.
pub struct Connection<S, const CHUNK_SIZE: usize = DEFAULT_CHUNK_SIZE> {
    stream: S,
    buffer: Vec<u8>,
    disconnected: bool,
    header_finder: Finder,
}

impl<S: Read + Write, const CHUNK_SIZE: usize> Connection<S, CHUNK_SIZE> {
    #[inline]
    fn poll(&mut self) -> io::Result<()> {
        if self.disconnected {
            return Err(io::Error::new(ErrorKind::NotConnected, "connection closed"));
        }
        let mut chunk = [0u8; CHUNK_SIZE];
        match self.stream.read(&mut chunk).no_block() {
            Ok(read) => {
                if read > 0 {
                    self.buffer.extend_from_slice(&chunk[..read]);
                }
                Ok(())
            }
            Err(err) => {
                self.disconnected = true;
                Err(err)
            }
        }
    }
}

impl<S: Write, const CHUNK_SIZE: usize> Write for Connection<S, CHUNK_SIZE> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl<S, const CHUNK_SIZE: usize> Connection<S, CHUNK_SIZE> {
    /// Creates a new connection wrapper around the provided stream.
    ///
    /// Initializes a read buffer with capacity equal to `CHUNK_SIZE` and sets up
    /// the HTTP header boundary finder for parsing responses.
    ///
    /// # Arguments
    ///
    /// * `stream` - The underlying I/O stream to wrap
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use boomnet::http::Connection;
    /// use boomnet::stream::tcp::TcpStream;
    ///
    /// let tcp = TcpStream::try_from(("127.0.0.1", 4222)).unwrap();
    /// let connection = Connection::<_, 1024>::new(tcp);
    /// ```
    #[inline]
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(CHUNK_SIZE),
            disconnected: false,
            header_finder: Finder::new(b"\r\n\r\n"),
        }
    }

    /// Returns whether the connection has been marked as disconnected.
    #[inline]
    pub const fn is_disconnected(&self) -> bool {
        self.disconnected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_insert_headers() {
        let mut headers = Headers::default();

        headers["hello"] = "world";
        headers["foo"] = "bar";

        let mut iter = headers.iter();

        let (key, value) = iter.next().unwrap();
        assert_eq!((&"hello", &"world"), (key, value));
        assert_eq!("world", headers["hello"]);

        let (key, value) = iter.next().unwrap();
        assert_eq!((&"foo", &"bar"), (key, value));
        assert_eq!("bar", headers["foo"]);

        assert!(iter.next().is_none());
    }
}
