//! Various stream implementations on top of which protocol can be applied.

use crate::inet::{FromSocketAddr, IntoNetworkInterface, ToSocketAddr};
use crate::service::select::Selectable;
use pnet::datalink::NetworkInterface;
use socket2::{Domain, Protocol, Socket, Type};
use std::fmt::{Debug, Display, Formatter};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::{io, vec};
use url::{ParseError, Url};

pub mod buffer;
pub mod file;
#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub mod io_uring;
#[cfg(all(target_os = "linux", feature = "ktls"))]
pub mod ktls;
#[cfg(feature = "mio")]
pub mod mio;
pub mod record;
pub mod replay;
pub mod tcp;
#[cfg(any(feature = "rustls", feature = "openssl"))]
pub mod tls;

/// Advisory hint indicating whether an immediate read attempt may make progress.
///
/// A `true` value is not a guarantee that [`std::io::Read::read`] will return
/// data. A `false` value allows protocol layers to avoid an otherwise known-empty
/// read attempt.
pub trait ReadHint {
    /// Returns whether an immediate read attempt may make progress.
    fn read_hint(&self) -> bool;
}

#[cfg(target_os = "linux")]
const EINPROGRESS: i32 = 115;
#[cfg(target_os = "macos")]
const EINPROGRESS: i32 = 36;

/// Additional socket options not exposed by [`socket2::Socket`].
pub trait SocketExt {
    /// Enable or disable Linux's `SO_PREFER_BUSY_POLL` socket option.
    #[cfg(target_os = "linux")]
    fn set_prefer_busy_poll(&self, prefer: bool) -> io::Result<()>;

    /// Set Linux's `SO_BUSY_POLL_BUDGET` NAPI packet-processing budget.
    ///
    /// Raising the budget requires the `CAP_NET_ADMIN` capability.
    #[cfg(target_os = "linux")]
    fn set_busy_poll_budget(&self, budget: u16) -> io::Result<()>;
}

impl SocketExt for Socket {
    #[cfg(target_os = "linux")]
    fn set_prefer_busy_poll(&self, prefer: bool) -> io::Result<()> {
        set_socket_int_option(self, libc::SO_PREFER_BUSY_POLL, prefer.into())
    }

    #[cfg(target_os = "linux")]
    fn set_busy_poll_budget(&self, budget: u16) -> io::Result<()> {
        set_socket_int_option(self, libc::SO_BUSY_POLL_BUDGET, budget.into())
    }
}

#[cfg(target_os = "linux")]
fn set_socket_int_option(socket: &Socket, option: libc::c_int, value: libc::c_int) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: `socket` supplies a valid descriptor and `value` is the integer
    // payload required by the supplied SOL_SOCKET option.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            std::ptr::from_ref(&value).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

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
        let socket_addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::other("unable to resolve socket address"))?;

        // create a socket but do not connect yet
        let socket = Socket::new(
            match &socket_addr {
                SocketAddr::V4(_) => Domain::IPV4,
                SocketAddr::V6(_) => Domain::IPV6,
            },
            Type::STREAM,
            Some(Protocol::TCP),
        )?;
        socket.set_nonblocking(true)?;
        socket.set_tcp_nodelay(true)?;
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
        match socket.connect(&socket_addr.into()) {
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

    fn make_writable(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn make_readable(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ReadHint for TcpStream {
    #[inline]
    fn read_hint(&self) -> bool {
        true
    }
}

pub trait ConnectionInfoProvider {
    fn connection_info(&self) -> &ConnectionInfo;
}

type SocketConfig = dyn Fn(&Socket) -> io::Result<()> + Send + Sync + 'static;

/// TCP stream connection info.
#[derive(Clone, Default)]
pub struct ConnectionInfo {
    host: String,
    port: u16,
    net_iface: Option<SocketAddr>,
    net_iface_name: Option<String>,
    cpu: Option<usize>,
    socket_config: Option<Arc<SocketConfig>>,
}

impl Debug for ConnectionInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionInfo")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("net_iface", &self.net_iface)
            .field("net_iface_name", &self.net_iface_name)
            .field("cpu", &self.cpu)
            .field("socket_config", &self.socket_config.as_ref().map(|_| "<closure>"))
            .finish()
    }
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
            net_iface_name: None,
            cpu: None,
            socket_config: None,
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

impl From<(&str, u16)> for ConnectionInfo {
    fn from(host_and_port: (&str, u16)) -> Self {
        let (host, port) = host_and_port;
        Self::new(host, port)
    }
}

impl ConnectionInfo {
    /// Create a new connection info from `host` and `port`.
    pub fn new(host: impl AsRef<str>, port: u16) -> Self {
        Self {
            host: host.as_ref().to_string(),
            port,
            net_iface: None,
            net_iface_name: None,
            cpu: None,
            socket_config: None,
        }
    }

    /// Add network interface using ip address. Will panic if invalid address provided.
    pub fn with_net_iface(self, net_iface: SocketAddr) -> Self {
        let nif = NetworkInterface::from_socket_addr(net_iface).expect("invalid network interface");
        Self {
            net_iface: Some(net_iface),
            net_iface_name: Some(nif.name),
            ..self
        }
    }

    /// Add network interface using the interface name. Will panic if no interface with that
    /// name can be found.
    pub fn with_net_iface_from_name(self, net_iface_name: &str) -> Self {
        let net_iface = net_iface_name
            .into_network_interface()
            .and_then(|iface| iface.to_socket_addr())
            .unwrap_or_else(|| panic!("invalid network interface: {net_iface_name}"));
        Self {
            net_iface: Some(net_iface),
            net_iface_name: Some(net_iface_name.to_owned()),
            ..self
        }
    }

    pub fn with_cpu(self, cpu: usize) -> Self {
        Self { cpu: Some(cpu), ..self }
    }

    /// Add a reusable custom action used to configure each newly-created socket.
    ///
    /// The action may capture owned state and is shared by cloned connection information.
    ///
    /// ```no_run
    /// use boomnet::stream::{ConnectionInfo, SocketExt};
    ///
    /// let busy_poll_micros = 50;
    /// let connection = ConnectionInfo::new("stream.binance.com", 443).with_socket_config(
    ///     move |socket| {
    ///         socket.set_busy_poll(busy_poll_micros)?;
    ///         #[cfg(target_os = "linux")]
    ///         {
    ///             socket.set_prefer_busy_poll(true)?;
    ///             socket.set_busy_poll_budget(64)?;
    ///         }
    ///         Ok(())
    ///     },
    /// );
    /// ```
    pub fn with_socket_config<F>(self, socket_config: F) -> Self
    where
        F: Fn(&Socket) -> io::Result<()> + Send + Sync + 'static,
    {
        Self {
            socket_config: Some(Arc::new(socket_config)),
            ..self
        }
    }

    /// Get host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get network interface address.
    pub fn net_iface(&self) -> Option<SocketAddr> {
        self.net_iface
    }

    /// Get network interface name.
    pub fn net_iface_name_as_str(&self) -> Option<&str> {
        self.net_iface_name.as_deref()
    }

    /// Convert to tcp stream. This will perform DNS address resolution.
    pub fn into_tcp_stream(self) -> io::Result<tcp::TcpStream> {
        let stream = TcpStream::bind_and_connect_with_socket_config(&self, self.net_iface, self.cpu, |socket| {
            self.socket_config.as_ref().map_or(Ok(()), |f| f(socket))
        })?;
        Ok(tcp::TcpStream::new(stream, self))
    }

    /// Convert to tcp stream using already resolved address.
    pub fn into_tcp_stream_with_addr(self, addr: SocketAddr) -> io::Result<tcp::TcpStream> {
        let stream = TcpStream::bind_and_connect_with_socket_config(addr, self.net_iface, self.cpu, |socket| {
            self.socket_config.as_ref().map_or(Ok(()), |f| f(socket))
        })?;
        Ok(tcp::TcpStream::new(stream, self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn socket_config_can_capture_state_and_is_shared_by_clones() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&calls);
        let connection_info = ConnectionInfo::new("localhost", 443).with_socket_config(move |_| {
            observed_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        let cloned = connection_info.clone();
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();

        connection_info.socket_config.as_ref().unwrap()(&socket).unwrap();
        cloned.socket_config.as_ref().unwrap()(&socket).unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
