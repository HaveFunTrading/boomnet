use crate::stream::record::{IntoRecordedStream, RecordedStream};
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

pub struct HttpClient<C: ConnectionPool> {
    connection_pool: Rc<RefCell<C>>,
    header_finder: Rc<Finder>,
}

impl<C: ConnectionPool> HttpClient<C> {
    pub fn new(connection_pool: C) -> HttpClient<C> {
        Self {
            connection_pool: Rc::new(RefCell::new(connection_pool)),
            header_finder: Rc::new(Finder::new(b"\r\n\r\n")),
        }
    }

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

    pub fn new_request(
        &mut self,
        method: Method,
        path: impl AsRef<str>,
        body: Option<&[u8]>,
    ) -> io::Result<HttpRequest<C>> {
        self.new_request_with_headers(method, path, body, |_| 0)
    }
}

pub trait ConnectionPool: Sized {
    type Stream: Read + Write;

    fn host(&self) -> &str;

    fn acquire(&mut self) -> io::Result<Option<Self::Stream>>;

    fn release(&mut self, stream: Option<Self::Stream>);
}

pub struct SingleTlsConnectionPool {
    connection_info: ConnectionInfo,
    stream: Option<RecordedStream<TlsStream<TcpStream>>>,
    has_active_connection: bool,
}

impl SingleTlsConnectionPool {
    pub fn new(connection_info: impl Into<ConnectionInfo>) -> SingleTlsConnectionPool {
        Self {
            connection_info: connection_info.into(),
            stream: None,
            has_active_connection: false,
        }
    }
}

impl ConnectionPool for SingleTlsConnectionPool {
    type Stream = RecordedStream<TlsStream<TcpStream>>;

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
                    .into_default_recorded_stream();
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

pub struct HttpRequest<C: ConnectionPool> {
    stream: Option<C::Stream>,
    connection_pool: Option<Rc<RefCell<C>>>,
    buffer: Vec<u8>,
    read: usize,
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
            connection_pool: Some(connection_pool),
            buffer: Vec::with_capacity(1024),
            read: 0,
            state: State::ReadingHeaders,
            header_finder,
        })
    }

    #[inline]
    pub fn block(mut self) -> io::Result<(u16, String, String)> {
        loop {
            if let Some((status_code, headers, body)) = self.poll()? {
                return Ok((status_code, headers.to_owned(), body.to_owned()));
            }
        }
    }

    // read a chunk - disposable

    /// Returns (status_code, headers, body).
    pub fn poll(&mut self) -> io::Result<Option<(u16, &str, &str)>> {
        if let Some(ref mut stream) = self.stream {
            match self.state {
                State::ReadingHeaders | State::ReadingBody { .. } => {
                    let mut chunk = [0u8; 1024];
                    match stream.read(&mut chunk).no_block() {
                        Ok(read) => {
                            if read > 0 {
                                self.buffer.extend_from_slice(&chunk[..read]);
                            }
                            self.read += read;
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
                    if self.read >= 4 {
                        if let Some(headers_end) = self.header_finder.find(&self.buffer[..self.read], b"\r\n\r\n") {
                            let header_len = headers_end + 4;
                            let header_slice = &self.buffer[..header_len];
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
                    if self.read >= total_len {
                        println!("{} {}", self.read, self.buffer.len());
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
                    let (headers, body) = self.buffer[..self.read].split_at(header_len);
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
        if let Some(pool) = self.connection_pool.as_mut() {
            pool.borrow_mut().release(self.stream.take());
        }
    }
}
