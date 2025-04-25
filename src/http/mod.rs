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

use crate::stream::buffer::{BufferedStream, IntoBufferedStream};
use crate::stream::tcp::TcpStream;
use crate::stream::tls::{IntoTlsStream, TlsConfigExt, TlsStream};
use crate::stream::ConnectionInfo;
use crate::util::NoBlock;
use http::Method;
use httparse::{Response, EMPTY_HEADER};
use memchr::arch::all::rabinkarp::Finder;
use std::cell::RefCell;
use std::io;
use std::io::{ErrorKind, Read, Write};
use std::rc::Rc;

type HttpTlsConnection = Connection<BufferedStream<TlsStream<TcpStream>>>;

/// A generic HTTP client that uses a pooled connection strategy.
pub struct HttpClient<C: ConnectionPool> {
    connection_pool: Rc<RefCell<C>>,
    header_finder: Rc<Finder>,
}

impl<C: ConnectionPool> HttpClient<C> {
    /// Create a new HTTP client from the provided pool.
    pub fn new(connection_pool: C) -> HttpClient<C> {
        Self {
            connection_pool: Rc::new(RefCell::new(connection_pool)),
            header_finder: Rc::new(Finder::new(b"\r\n\r\n")),
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
    ///         hdrs[0] = ("X-Custom", "Value");
    ///         1
    ///     }
    /// ).unwrap();
    /// ```
    pub fn new_request_with_headers<F>(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
        builder: F,
    ) -> io::Result<HttpRequest<C>>
    where
        F: FnOnce(&mut [(&str, &str)]) -> usize,
    {
        let mut headers = [("", ""); 32];
        let count = builder(&mut headers);
        let conn = self
            .connection_pool
            .borrow_mut()
            .acquire()?
            .ok_or_else(|| io::Error::other("no available connection"))?;
        let request = HttpRequest::new(
            method,
            path,
            body,
            &headers[..count],
            conn,
            self.connection_pool.clone(),
            self.header_finder.clone(),
        )?;
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
    ) -> io::Result<HttpRequest<C>> {
        self.new_request_with_headers(method, path, body, |_| 0)
    }
}

/// Trait defining a pool of reusable connections.
pub trait ConnectionPool: Sized {
    /// Underlying stream type.
    type Stream: Read + Write;

    /// Turn this connection pool into http client.
    fn into_http_client(self) -> HttpClient<Self> {
        HttpClient::new(self)
    }

    /// Hostname for requests.
    fn host(&self) -> &str;

    /// Acquire next free connection, if available.
    fn acquire(&mut self) -> io::Result<Option<Connection<Self::Stream>>>;

    /// Release a connection back into the pool.
    fn release(&mut self, stream: Option<Connection<Self::Stream>>);
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
        if let Some(mut conn) = conn {
            if !conn.disconnected {
                let _ = self.conn.insert(conn);
            }
        }
    }
}

/// Represents an in-flight HTTP exchange.
pub struct HttpRequest<C: ConnectionPool> {
    conn: Option<Connection<C::Stream>>,
    connection_pool: Rc<RefCell<C>>,
    state: State,
    header_finder: Rc<Finder>,
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

impl<C: ConnectionPool> HttpRequest<C> {
    fn new(
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
        mut conn: Connection<C::Stream>,
        connection_pool: Rc<RefCell<C>>,
        header_finder: Rc<Finder>,
    ) -> io::Result<HttpRequest<C>> {
        conn.write_all(method.as_str().as_bytes())?;
        conn.write_all(b" ")?;
        conn.write_all(path.as_ref().as_bytes())?;
        conn.write_all(b" HTTP/1.1\r\nHost: ")?;
        conn.write_all(connection_pool.borrow().host().as_bytes())?;
        if !headers.is_empty() {
            conn.write_all(b"\r\n")?;
            for header in headers {
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
            connection_pool,
            state: State::ReadingHeaders,
            header_finder,
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
    ///         hdrs[0] = ("X-Custom", "Value");
    ///         1
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
                        if let Some(headers_end) = self.header_finder.find(&conn.buffer, b"\r\n\r\n") {
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

impl<C: ConnectionPool> Drop for HttpRequest<C> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.as_mut() {
            conn.buffer.clear();
        }
        self.connection_pool.borrow_mut().release(self.conn.take());
    }
}

pub struct Connection<S> {
    stream: S,
    buffer: Vec<u8>,
    disconnected: bool,
}

impl<S: Read + Write> Connection<S> {
    #[inline]
    fn poll(&mut self) -> io::Result<()> {
        if self.disconnected {
            return Err(io::Error::new(ErrorKind::NotConnected, "connection closed"));
        }
        let mut chunk = [0u8; 1024];
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

impl<S: Write> Write for Connection<S> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl<S> Connection<S> {
    #[inline]
    fn new(stream: S) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(1024),
            disconnected: false,
        }
    }
}
