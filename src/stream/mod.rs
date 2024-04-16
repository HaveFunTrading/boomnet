//! Various stream implementations on top of which protocol can be applied.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};

use socket2::{Domain, Protocol, Socket, Type};

use crate::select::Selectable;

pub mod buffer;
pub mod file;
#[cfg(feature = "mio")]
pub mod mio;
pub mod record;
pub mod replay;
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
        A: ToSocketAddrs;
}

impl BindAndConnect for TcpStream {
    #[allow(unused_variables)]
    fn bind_and_connect<A>(addr: A, net_iface: Option<SocketAddr>, cpu: Option<usize>) -> io::Result<TcpStream>
    where
        A: ToSocketAddrs,
    {
        // create a socket but do not connect yet
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_nonblocking(true)?;
        socket.set_nodelay(true)?;

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
