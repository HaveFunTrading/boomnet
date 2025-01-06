//! Various stream implementations on top of which protocol can be applied.

use crate::inet::ToSocketAddr;
use crate::select::Selectable;
use socket2::{Domain, Protocol, Socket, Type};
use std::fmt::{Display, Formatter};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::{io, vec};
use url::{ParseError, Url};

pub mod buffer;
pub mod file;
#[cfg(feature = "mio")]
pub mod mio;
pub mod record;
pub mod replay;
pub mod tcp;
#[cfg(any(feature = "tls-webpki", feature = "tls-native"))]
pub mod tls;

#[cfg(target_os = "linux")]
const EINPROGRESS: i32 = 115;
#[cfg(target_os = "macos")]
const EINPROGRESS: i32 = 36;

/// Trait to create `TcpStream` and optionally bind it to a specific network interface and/or cpu
/// before connecting.
///
/// # Examples
///
/// Bind to a specific network interface.
///
/// ```no_run
/// use std::net::TcpStream;
/// use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
/// use boomnet::stream::BindAndConnect;
///
/// let inet = "eth1".into_network_interface().and_then(|inet| inet.to_socket_addr());
/// let stream = TcpStream::bind_and_connect("stream.binance.com", inet, None).unwrap();
/// ```
///
/// Set `SO_INCOMING_CPU` affinity.
///
/// ```no_run
/// use std::net::TcpStream;
/// use boomnet::stream::BindAndConnect;
///
/// let stream = TcpStream::bind_and_connect("stream.binance.com", None, Some(2)).unwrap();
/// ```
pub trait BindAndConnect {
    /// Creates `TcpStream` and optionally binds it to network interface and/or CPU before
    /// connecting.
    ///
    /// # Examples
    ///
    /// Bind to a specific network interface.
    ///
    /// ```no_run
    /// use std::net::TcpStream;
    /// use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
    /// use boomnet::stream::BindAndConnect;
    ///
    /// let inet = "eth1".into_network_interface().and_then(|inet| inet.to_socket_addr());
    /// let stream = TcpStream::bind_and_connect("stream.binance.com", inet, None).unwrap();
    /// ```
    ///
    /// Set `SO_INCOMING_CPU` affinity.
    ///
    /// ```no_run
    /// use std::net::TcpStream;
    /// use boomnet::stream::BindAndConnect;
    ///
    /// let stream = TcpStream::bind_and_connect("stream.binance.com", None, Some(2)).unwrap();
    /// ```
    fn bind_and_connect<A>(addr: A, net_iface: Option<SocketAddr>, cpu: Option<usize>) -> io::Result<TcpStream>
    where
        A: ToSocketAddrs,
    {
        Self::bind_and_connect_with_socket_config(addr, net_iface, cpu, |_| Ok(()))
    }

    /// Creates `TcpStream` and optionally binds it to network interface and/or CPU before
    /// connecting. This also accepts user defined `socket_config` closure that will be applied
    /// to the socket.
    ///
    /// # Examples
    ///
    /// Bind to a specific network interface.
    ///
    /// ```no_run
    /// use std::net::TcpStream;
    /// use boomnet::inet::{IntoNetworkInterface, ToSocketAddr};
    /// use boomnet::stream::BindAndConnect;
    ///
    /// let inet = "eth1".into_network_interface().and_then(|inet| inet.to_socket_addr());
    /// let stream = TcpStream::bind_and_connect("stream.binance.com", inet, None).unwrap();
    /// ```
    ///
    /// Set `SO_INCOMING_CPU` affinity.
    ///
    /// ```no_run
    /// use std::net::TcpStream;
    /// use boomnet::stream::BindAndConnect;
    ///
    /// let stream = TcpStream::bind_and_connect("stream.binance.com", None, Some(2)).unwrap();
    /// ```
    ///
    /// Use `socket_config` to enable additional socket options.
    ///
    /// ```no_run
    /// use std::net::TcpStream;
    /// use boomnet::stream::BindAndConnect;
    ///
    /// let stream = TcpStream::bind_and_connect_with_socket_config("stream.binance.com", None, Some(2), |socket| {
    ///     socket.set_reuse_address(true)?;
    ///     Ok(())
    /// }).unwrap();
    /// ```
    ///
    fn bind_and_connect_with_socket_config<A, F>(
        addr: A,
        net_iface: Option<SocketAddr>,
        cpu: Option<usize>,
        socket_config: F,
    ) -> io::Result<TcpStream>
    where
        A: ToSocketAddrs,
        F: FnOnce(&Socket) -> io::Result<()>;
}

impl BindAndConnect for TcpStream {
    #[allow(unused_variables)]
    fn bind_and_connect_with_socket_config<A, F>(
        addr: A,
        net_iface: Option<SocketAddr>,
        cpu: Option<usize>,
        socket_config: F,
    ) -> io::Result<TcpStream>
    where
        A: ToSocketAddrs,
        F: FnOnce(&Socket) -> io::Result<()>,
    {
        // create a socket but do not connect yet
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_nonblocking(true)?;
        socket.set_nodelay(true)?;
        socket.set_keepalive(true)?;

        // apply custom options
        socket_config(&socket)?;

        // optionally bind to a specific network interface
        if let Some(addr) = net_iface {
            socket.bind(&addr.into())?;
        }

        // optionally set rx cpu affinity (only on linux)
        #[cfg(target_os = "linux")]
        if let Some(cpu_affinity) = cpu {
            socket.set_cpu_affinity(cpu_affinity)?;
        }

        // connect to the remote endpoint
        // we can ignore EINPROGRESS error due to non-blocking socket
        match socket.connect(
            &addr
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| io::Error::other("unable to resolve socket address"))?
                .into(),
        ) {
            Ok(()) => Ok(socket.into()),
            Err(err) if err.raw_os_error() == Some(EINPROGRESS) => Ok(socket.into()),
            Err(err) => Err(err),
        }
    }
}

impl Selectable for TcpStream {
    fn connected(&mut self) -> io::Result<bool> {
        Ok(true)
    }

    fn make_writable(&mut self) {
        // no-op
    }

    fn make_readable(&mut self) {
        // no-op
    }
}

/// TCP stream connection info.
#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    host: String,
    port: u16,
    net_iface: Option<SocketAddr>,
    cpu: Option<usize>,
}

impl ToSocketAddrs for ConnectionInfo {
    type Iter = vec::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter> {
        format!("{}:{}", self.host, self.port).to_socket_addrs()
    }
}

impl Display for ConnectionInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl TryFrom<Url> for ConnectionInfo {
    type Error = io::Error;

    fn try_from(url: Url) -> Result<Self, Self::Error> {
        Ok(ConnectionInfo {
            host: url
                .host_str()
                .ok_or_else(|| io::Error::other("host not present"))?
                .to_owned(),
            port: url
                .port_or_known_default()
                .ok_or_else(|| io::Error::other("port not present"))?,
            net_iface: None,
            cpu: None,
        })
    }
}

impl TryFrom<Result<Url, ParseError>> for ConnectionInfo {
    type Error = io::Error;

    fn try_from(result: Result<Url, ParseError>) -> Result<Self, Self::Error> {
        match result {
            Ok(url) => Ok(url.try_into()?),
            Err(err) => Err(io::Error::other(err)),
        }
    }
}

impl ConnectionInfo {
    pub fn new(host: impl AsRef<str>, port: u16) -> Self {
        Self {
            host: host.as_ref().to_string(),
            port,
            net_iface: None,
            cpu: None,
        }
    }

    pub fn with_net_iface(self, net_iface: SocketAddr) -> Self {
        Self {
            net_iface: Some(net_iface),
            ..self
        }
    }

    pub fn with_cpu(self, cpu: usize) -> Self {
        Self { cpu: Some(cpu), ..self }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn into_tcp_stream(self) -> io::Result<tcp::TcpStream> {
        let stream = TcpStream::bind_and_connect(&self, self.net_iface, self.cpu)?;
        Ok(tcp::TcpStream::new(stream, self))
    }
}

pub trait ConnectionInfoProvider {
    fn connection_info(&self) -> &ConnectionInfo;
}
