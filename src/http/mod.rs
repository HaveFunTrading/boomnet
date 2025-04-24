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
        let stream = self
            .connection_pool
            .borrow_mut()
            .acquire()?
            .ok_or_else(|| io::Error::other("no available connection"))?;
        let request = HttpRequest::new(
            method,
            path,
            body,
            &headers[..count],
            stream,
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
    /// Turn this connection pool into http client.
    fn into_http_client(self) -> HttpClient<Self> {
        HttpClient::new(self)
    }

    /// Underlying stream type.
    type Stream: Read + Write;

    /// Hostname for requests.
    fn host(&self) -> &str;

    /// Acquire next free connection, if available.
    fn acquire(&mut self) -> io::Result<Option<Self::Stream>>;

    /// Release a connection back into the pool.
    fn release(&mut self, stream: Option<Self::Stream>);
}

/// A single-connection pool over TLS, reconnecting on demand.
pub struct SingleTlsConnectionPool {
    connection_info: ConnectionInfo,
    stream: Option<BufferedStream<TlsStream<TcpStream>>>,
    has_active_connection: bool,
}

impl SingleTlsConnectionPool {
    /// Build a new TLS pool for the given connection info.
    pub fn new(connection_info: impl Into<ConnectionInfo>) -> SingleTlsConnectionPool {
        Self {
            connection_info: connection_info.into(),
            stream: None,
            has_active_connection: false,
        }
    }
}

impl ConnectionPool for SingleTlsConnectionPool {
    type Stream = BufferedStream<TlsStream<TcpStream>>;

    fn host(&self) -> &str {
        self.connection_info.host()
    }

    fn acquire(&mut self) -> io::Result<Option<Self::Stream>> {
        match (self.stream.take(), self.has_active_connection) {
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
                Ok(Some(stream))
            }
        }
    }

    fn release(&mut self, stream: Option<Self::Stream>) {
        self.has_active_connection = false;
        match stream {
            None => {}
            Some(stream) => {
                let _ = self.stream.insert(stream);
            }
        }
    }
}

/// Represents an in-flight HTTP exchange.
pub struct HttpRequest<C: ConnectionPool> {
    stream: Option<C::Stream>,
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
        mut stream: C::Stream,
        connection_pool: Rc<RefCell<C>>,
        header_finder: Rc<Finder>,
    ) -> io::Result<HttpRequest<C>> {
        stream.write_all(method.as_str().as_bytes())?;
        stream.write_all(b" ")?;
        stream.write_all(path.as_ref().as_bytes())?;
        stream.write_all(b" HTTP/1.1\r\nHost: ")?;
        stream.write_all(connection_pool.borrow().host().as_bytes())?;
        if !headers.is_empty() {
            stream.write_all(b"\r\n")?;
            for header in headers {
                stream.write_all(header.0.as_bytes())?;
                stream.write_all(b": ")?;
                stream.write_all(header.1.as_bytes())?;
                stream.write_all(b"\r\n")?;
            }
            if let Some(body) = body {
                stream.write_all(b"Content-Length: ")?;
                let mut buf = itoa::Buffer::new();
                stream.write_all(buf.format(body.len()).as_bytes())?;
                stream.write_all(b"\r\n")?;
            }
            stream.write_all(b"\r\n")?;
        } else if let Some(body) = body {
            stream.write_all(b"\r\n")?;
            stream.write_all(b"Content-Length: ")?;
            let mut buf = itoa::Buffer::new();
            stream.write_all(buf.format(body.as_ref().len()).as_bytes())?;
            stream.write_all(b"\r\n\r\n")?;
        } else {
            stream.write_all(b"\r\n\r\n")?;
        }
        if let Some(body) = body {
            stream.write_all(body)?;
        }
        stream.flush()?;
        Ok(Self {
            stream: Some(stream),
            connection_pool,
            state: State::ReadingHeaders,
            header_finder,
        })
    }

    /// Block until the full response is available.
    #[inline]
    pub fn block(mut self) -> io::Result<(u16, String, String)> {
        let mut buffer = Vec::with_capacity(1024);
        loop {
            if let Some((status_code, headers, body)) = self.poll(&mut buffer)? {
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
    /// let mut buffer = Vec::with_capacity(1024);
    /// loop {
    ///     if let Some((status_code, headers, body)) = request.poll(&mut buffer).unwrap() {
    ///         println!("{}", status_code);
    ///         println!("{}", headers);
    ///         println!("{}", body);
    ///         break;
    ///     }
    /// }
    ///
    /// ```
    pub fn poll<'a>(&mut self, buffer: &'a mut Vec<u8>) -> io::Result<Option<(u16, &'a str, &'a str)>> {
        if let Some(ref mut stream) = self.stream {
            match self.state {
                State::ReadingHeaders | State::ReadingBody { .. } => {
                    let mut chunk = [0u8; 1024];
                    match stream.read(&mut chunk).no_block() {
                        Ok(read) => {
                            if read > 0 {
                                buffer.extend_from_slice(&chunk[..read]);
                            }
                        }
                        Err(err) => {
                            let _ = self.stream.take();
                            return Err(err);
                        }
                    }
                }
                State::Done { .. } => {}
            }
            match self.state {
                State::ReadingHeaders => {
                    if buffer.len() >= 4 {
                        if let Some(headers_end) = self.header_finder.find(buffer, b"\r\n\r\n") {
                            let header_len = headers_end + 4;
                            let header_slice = &buffer[..header_len];
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
                    if buffer.len() >= total_len {
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
                    let (headers, body) = buffer.split_at(header_len);
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
        self.connection_pool.borrow_mut().release(self.stream.take());
    }
}
