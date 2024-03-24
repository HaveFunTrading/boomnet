//! Various stream implementations on top of which protocol can be applied.

use std::io;
use std::net::{SocketAddr, TcpStream};

use crate::select::Selectable;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

pub mod file;
#[cfg(feature = "mio")]
pub mod mio;
pub mod record;
pub mod replay;
#[cfg(feature = "tls")]
pub mod tls;

#[cfg(target_os = "linux")]
const EINPROGRESS: i32 = 115;
#[cfg(target_os = "macos")]
const EINPROGRESS: i32 = 36;

pub trait BindAndConnect {
    fn bind_and_connect(addr: SocketAddr, net_iface: Option<SocketAddr>, cpu: Option<usize>) -> io::Result<TcpStream>;
}

impl BindAndConnect for TcpStream {
    #[allow(unused_variables)]
    fn bind_and_connect(addr: SocketAddr, net_iface: Option<SocketAddr>, cpu: Option<usize>) -> io::Result<TcpStream> {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_nonblocking(true)?;
        socket.set_nodelay(true)?;

        // optionally bind to a specific network interface
        if let Some(addr) = net_iface {
            let addr = SockAddr::from(addr);
            socket.bind(&addr)?;
        }

        // optionally set rx cpu affinity
        #[cfg(target_os = "linux")]
        if let Some(cpu_affinity) = cpu {
            socket.set_cpu_affinity(cpu_affinity)?;
        }

        // we can ignore EINPROGRESS due to non-blocking socket
        match socket.connect(&SockAddr::from(addr)) {
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
