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

/// Request-scoped container for outgoing headers.
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
}

/// A generic HTTP client that uses a pooled connection strategy.
pub struct HttpClient<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize = DEFAULT_CHUNK_SIZE> {
    connection_pool: Rc<RefCell<C>>,
}

impl<C: ConnectionPool<CHUNK_SIZE>, const CHUNK_SIZE: usize> HttpClient<C, CHUNK_SIZE> {
    /// Create a new HTTP client from the provided pool.
    pub fn new(connection_pool: C) -> HttpClient<C, CHUNK_SIZE> {
        Self {
            connection_pool: Rc::new(RefCell::new(connection_pool)),
        }
    }

    /// Prepare a request with custom headers and optional body.
    ///
    /// Returns an error with [`io::ErrorKind::WouldBlock`] if the pool is unable to allocate a connection.
    /// Other errors may be returned while acquiring the connection or preparing the request.
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
    #[inline]
    pub fn new_request_with_headers<'headers, F>(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
        builder: F,
    ) -> io::Result<HttpRequest<C, CHUNK_SIZE>>
    where
        F: FnOnce(&mut Headers<'headers>),
    {
        self.try_new_request_with_headers(method, path, body, builder)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "no available connection in the pool"))
    }

    /// Prepare a request with no additional headers and optional body.
    ///
    /// Returns an error with [`io::ErrorKind::WouldBlock`] if the pool is unable to allocate a connection.
    /// Other errors may be returned while acquiring the connection or preparing the request.
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
    #[inline]
    pub fn new_request(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
    ) -> io::Result<HttpRequest<C, CHUNK_SIZE>> {
        self.new_request_with_headers(method, path, body, |_| {})
    }

    /// Try to prepare a request with custom headers and optional body.
    ///
    /// Returns `Ok(None)` if the pool is unable to allocate a connection. In that case, no request is
    /// prepared and `builder` is not called.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use http::Method;
    /// use boomnet::http::{ConnectionPool, SingleTlsConnectionPool};
    /// use boomnet::stream::ConnectionInfo;
    ///
    /// let mut client = SingleTlsConnectionPool::new(ConnectionInfo::new("example.com", 443)).into_http_client();
    ///
    /// match client.try_new_request_with_headers(
    ///     Method::POST,
    ///     "/submit",
    ///     Some(b"data"),
    ///     |hdrs| {
    ///         hdrs["X-Custom"] = "Value";
    ///     },
    /// ).unwrap() {
    ///     Some(request) => {
    ///         let response = request.block().unwrap();
    ///     }
    ///     None => {
    ///         // Try again after a connection becomes available.
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn try_new_request_with_headers<'headers, F>(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
        builder: F,
    ) -> io::Result<Option<HttpRequest<C, CHUNK_SIZE>>>
    where
        F: FnOnce(&mut Headers<'headers>),
    {
        let Some(conn) = self.connection_pool.borrow_mut().acquire()? else {
            return Ok(None);
        };

        let mut headers = Headers::default();
        builder(&mut headers);

        let request = match HttpRequest::new(method, path, body, &headers, conn, self.connection_pool.clone()) {
            Ok(request) => request,
            Err(error) => {
                // `acquire` has marked the connection as active, but no `HttpRequest` was
                // constructed to release it on drop. Discard the failed connection and make
                // the pool available again.
                self.connection_pool.borrow_mut().release(None);
                return Err(error);
            }
        };

        Ok(Some(request))
    }

    /// Try to prepare a request with no additional headers and optional body.
    ///
    /// Returns `Ok(None)` if the pool is unable to allocate a connection. In that case, no request is
    /// prepared.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use http::Method;
    /// use boomnet::http::{ConnectionPool, SingleTlsConnectionPool};
    /// use boomnet::stream::ConnectionInfo;
    ///
    /// let mut client = SingleTlsConnectionPool::new(ConnectionInfo::new("example.com", 443)).into_http_client();
    ///
    /// match client.try_new_request(Method::GET, "/", None).unwrap() {
    ///     Some(request) => {
    ///         let response = request.block().unwrap();
    ///     }
    ///     None => {
    ///         // Try again after a connection becomes available.
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn try_new_request(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
    ) -> io::Result<Option<HttpRequest<C, CHUNK_SIZE>>> {
        self.try_new_request_with_headers(method, path, body, |_| {})
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
        if let Some(conn) = conn
            && !conn.disconnected
        {
            let _ = self.conn.insert(conn);
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
    ReadingChunkedBody {
        header_len: usize,
        status_code: u16,
    },
    Done {
        header_len: usize,
        status_code: u16,
    },
}

fn find_crlf(finder: &Finder, bytes: &[u8], start: usize) -> Option<usize> {
    finder.find(bytes.get(start..)?, b"\r\n").map(|offset| start + offset)
}

fn parse_chunk_size(line: &[u8]) -> io::Result<usize> {
    let size = line.split(|byte| *byte == b';').next().unwrap_or_default();
    if size.is_empty() {
        return Err(io::Error::new(ErrorKind::InvalidData, "missing HTTP chunk size"));
    }
    let size = std::str::from_utf8(size).map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    usize::from_str_radix(size, 16).map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

/// Return the encoded body length once the terminating chunk and trailers are complete.
fn chunked_body_len(finder: &Finder, body: &[u8]) -> io::Result<Option<usize>> {
    let mut offset = 0;
    loop {
        let Some(line_end) = find_crlf(finder, body, offset) else {
            return Ok(None);
        };
        let chunk_size = parse_chunk_size(&body[offset..line_end])?;
        offset = line_end + 2;

        if chunk_size == 0 {
            // A chunked message ends with an empty trailer line. Non-empty lines before it
            // are optional trailer fields.
            loop {
                let line_start = offset;
                let Some(line_end) = find_crlf(finder, body, offset) else {
                    return Ok(None);
                };
                offset = line_end + 2;
                if line_end == line_start {
                    return Ok(Some(offset));
                }
            }
        }

        let data_end = offset
            .checked_add(chunk_size)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "HTTP chunk size overflow"))?;
        let framed_end = data_end
            .checked_add(2)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "HTTP chunk size overflow"))?;
        if body.len() < framed_end {
            return Ok(None);
        }
        if body[data_end..framed_end] != *b"\r\n" {
            return Err(io::Error::new(ErrorKind::InvalidData, "HTTP chunk is missing trailing CRLF"));
        }
        offset = framed_end;
    }
}

fn decode_chunked_body(finder: &Finder, buffer: &mut Vec<u8>, header_len: usize) -> io::Result<()> {
    let mut read_offset = header_len;
    let mut write_offset = header_len;
    loop {
        let line_end = find_crlf(finder, buffer, read_offset)
            .ok_or_else(|| io::Error::new(ErrorKind::UnexpectedEof, "incomplete HTTP chunk size"))?;
        let chunk_size = parse_chunk_size(&buffer[read_offset..line_end])?;
        read_offset = line_end + 2;
        if chunk_size == 0 {
            buffer.truncate(write_offset);
            return Ok(());
        }

        let data_end = read_offset + chunk_size;
        buffer.copy_within(read_offset..data_end, write_offset);
        write_offset += chunk_size;
        read_offset = data_end + 2;
    }
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
                State::ReadingHeaders => conn.poll()?,
                State::ReadingBody {
                    header_len,
                    content_len,
                    ..
                } => {
                    let total_len = header_len
                        .checked_add(content_len)
                        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "HTTP content length overflow"))?;
                    if conn.buffer.len() < total_len {
                        conn.poll()?;
                    }
                }
                State::ReadingChunkedBody { header_len, .. } => {
                    if chunked_body_len(&conn.line_end_finder, &conn.buffer[header_len..])?.is_none() {
                        conn.poll()?;
                    }
                }
                State::Done { .. } => {}
            }
            match self.state {
                State::ReadingHeaders => {
                    if conn.buffer.len() >= 4
                        && let Some(headers_end) = conn.headers_end_finder.find(&conn.buffer, b"\r\n\r\n")
                    {
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
                                let mut chunked = false;
                                for header in resp.headers {
                                    if header.name.eq_ignore_ascii_case("Content-Length") {
                                        content_len = std::str::from_utf8(header.value)
                                            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?
                                            .parse()
                                            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                                    } else if header.name.eq_ignore_ascii_case("Transfer-Encoding") {
                                        let encoding = std::str::from_utf8(header.value)
                                            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                                        if !encoding.trim().eq_ignore_ascii_case("chunked") {
                                            return Err(io::Error::new(
                                                ErrorKind::InvalidData,
                                                format!("unsupported HTTP transfer encoding: {encoding}"),
                                            ));
                                        }
                                        chunked = true;
                                    }
                                }
                                self.state = if chunked {
                                    State::ReadingChunkedBody {
                                        header_len,
                                        status_code,
                                    }
                                } else {
                                    State::ReadingBody {
                                        header_len,
                                        content_len,
                                        status_code,
                                    }
                                };
                            }
                            Ok(httparse::Status::Partial) => {
                                return Err(io::Error::new(ErrorKind::InvalidData, "unable to parse headers"));
                            }
                            Err(err) => return Err(io::Error::new(ErrorKind::InvalidData, err)),
                        }
                    }
                }
                State::ReadingBody {
                    header_len,
                    content_len,
                    status_code,
                } => {
                    let total_len = header_len
                        .checked_add(content_len)
                        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "HTTP content length overflow"))?;
                    if conn.buffer.len() >= total_len {
                        self.state = State::Done {
                            header_len,
                            status_code,
                        };
                    }
                }
                State::ReadingChunkedBody {
                    header_len,
                    status_code,
                } => {
                    if chunked_body_len(&conn.line_end_finder, &conn.buffer[header_len..])?.is_some() {
                        decode_chunked_body(&conn.line_end_finder, &mut conn.buffer, header_len)?;
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

    /// Immediately terminate and consume this request.
    ///
    /// The request's connection is dropped instead of returned to the pool. Use this when the request
    /// can no longer complete or its connection may not be safe to reuse, such as after a timeout.
    pub fn invalidate(mut self) {
        self.conn.take();
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
    headers_end_finder: Finder,
    line_end_finder: Finder,
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
            headers_end_finder: Finder::new(b"\r\n\r\n"),
            line_end_finder: Finder::new(b"\r\n"),
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
    use std::cell::Cell;

    struct WriteErrorStream;

    impl Read for WriteErrorStream {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for WriteErrorStream {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::UnexpectedEof, "write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct TestConnectionPool {
        active: bool,
        releases: Rc<Cell<usize>>,
    }

    impl ConnectionPool for TestConnectionPool {
        type Stream = WriteErrorStream;

        fn host(&self) -> &str {
            "localhost"
        }

        fn acquire(&mut self) -> io::Result<Option<Connection<Self::Stream>>> {
            if self.active {
                return Ok(None);
            }
            self.active = true;
            Ok(Some(Connection::new(WriteErrorStream)))
        }

        fn release(&mut self, _conn: Option<Connection<Self::Stream>>) {
            self.active = false;
            self.releases.set(self.releases.get() + 1);
        }
    }

    struct FragmentedResponseStream {
        response: Vec<u8>,
        offset: usize,
    }

    impl Read for FragmentedResponseStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.offset == self.response.len() {
                return Ok(0);
            }
            let read = buffer.len().min(self.response.len() - self.offset);
            buffer[..read].copy_from_slice(&self.response[self.offset..self.offset + read]);
            self.offset += read;
            Ok(read)
        }
    }

    impl Write for FragmentedResponseStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FragmentedResponsePool(Option<FragmentedResponseStream>);

    impl ConnectionPool<8> for FragmentedResponsePool {
        type Stream = FragmentedResponseStream;

        fn host(&self) -> &str {
            "localhost"
        }

        fn acquire(&mut self) -> io::Result<Option<Connection<Self::Stream, 8>>> {
            Ok(self.0.take().map(Connection::new))
        }

        fn release(&mut self, connection: Option<Connection<Self::Stream, 8>>) {
            self.0 = connection.map(|connection| connection.stream);
        }
    }

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

    #[test]
    fn should_release_pool_when_request_construction_fails() {
        let releases = Rc::new(Cell::new(0));
        let pool = TestConnectionPool {
            active: false,
            releases: releases.clone(),
        };
        let mut client = HttpClient::new(pool);

        assert!(client.try_new_request(Method::GET, "/", None).is_err());
        assert_eq!(1, releases.get());

        // A second construction attempt reaches the stream instead of observing a busy pool.
        assert!(client.try_new_request(Method::GET, "/", None).is_err());
        assert_eq!(2, releases.get());
    }

    #[test]
    fn should_accept_request_scoped_header_values() {
        let releases = Rc::new(Cell::new(0));
        let pool = TestConnectionPool {
            active: false,
            releases,
        };
        let mut client = HttpClient::new(pool);
        let signature = String::from("request-scoped-signature");
        let timestamp = 1_790_027_709_u64.to_string();

        let result = client.try_new_request_with_headers(Method::GET, "/", None, |headers| {
            headers.insert("ACCESS-SIGN", &signature);
            headers.insert("ACCESS-TIMESTAMP", &timestamp);
        });

        assert!(result.is_err());
    }

    #[test]
    fn should_detect_and_decode_a_complete_chunked_body() {
        let headers = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let encoded = b"4\r\nWiki\r\n5;extension=yes\r\npedia\r\n0\r\nChecksum: value\r\n\r\n";
        let mut response = [headers.as_slice(), encoded.as_slice()].concat();
        let finder = Finder::new(b"\r\n");

        assert_eq!(Some(encoded.len()), chunked_body_len(&finder, &response[headers.len()..]).unwrap());
        decode_chunked_body(&finder, &mut response, headers.len()).unwrap();

        assert_eq!(b"Wikipedia", &response[headers.len()..]);
    }

    #[test]
    fn should_poll_a_fragmented_chunked_response_to_completion() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let pool = FragmentedResponsePool(Some(FragmentedResponseStream {
            response: response.to_vec(),
            offset: 0,
        }));
        let mut client: HttpClient<_, 8> = HttpClient::new(pool);

        let (status, headers, body) = client.new_request(Method::GET, "/", None).unwrap().block().unwrap();

        assert_eq!(200, status);
        assert!(headers.contains("Transfer-Encoding: chunked"));
        assert_eq!("Wikipedia", body);
    }

    #[test]
    fn should_wait_for_the_chunked_message_terminator() {
        let encoded = b"4\r\nWiki\r\n0\r\n\r\n";
        let finder = Finder::new(b"\r\n");

        for prefix_len in 0..encoded.len() {
            assert_eq!(None, chunked_body_len(&finder, &encoded[..prefix_len]).unwrap());
        }
        assert_eq!(Some(encoded.len()), chunked_body_len(&finder, encoded).unwrap());
    }

    #[test]
    fn should_reject_invalid_chunk_framing() {
        let error = chunked_body_len(&Finder::new(b"\r\n"), b"4\r\nWikipX").unwrap_err();

        assert_eq!(ErrorKind::InvalidData, error.kind());
    }
}
