use std::io;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

#[cfg(feature = "mio")]
use ::mio::{Interest, Registry, Token, event::Source};
use boomnet::service::select::{ActiveEndpointLookup, Selectable, Selector, SelectorToken};

struct Endpoint {
    socket: ::mio::net::TcpStream,
    writable: usize,
    readable: usize,
}

impl Selectable for Endpoint {
    fn connected(&mut self) -> io::Result<bool> {
        Ok(true)
    }
    fn make_writable(&mut self) -> io::Result<()> {
        self.writable += 1;
        Ok(())
    }
    fn make_readable(&mut self) -> io::Result<()> {
        self.readable += 1;
        Ok(())
    }
}

impl Source for Endpoint {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        self.socket.register(registry, token, interests)
    }
    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        self.socket.reregister(registry, token, interests)
    }
    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        self.socket.deregister(registry)
    }
}

impl AsRawFd for Endpoint {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}

struct Lookup {
    token: SelectorToken,
    endpoint: Endpoint,
}

impl ActiveEndpointLookup<Endpoint> for Lookup {
    fn get_active_mut(&mut self, token: SelectorToken) -> Option<&mut Endpoint> {
        (token == self.token).then_some(&mut self.endpoint)
    }
}

fn connected_pair() -> (Endpoint, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (socket, _) = listener.accept().unwrap();
    socket.set_nonblocking(true).unwrap();
    (
        Endpoint {
            socket: ::mio::net::TcpStream::from_std(socket),
            writable: 0,
            readable: 0,
        },
        peer,
    )
}

fn poll_until(selector: &mut impl Selector<Target = Endpoint>, lookup: &mut Lookup, ready: impl Fn(&Endpoint) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready(&lookup.endpoint) {
        selector.poll(lookup).unwrap();
        assert!(Instant::now() < deadline, "selector did not report readiness");
        std::thread::yield_now();
    }
}

fn connection_replacement(mut selector: impl Selector<Target = Endpoint>) {
    let (endpoint, mut peer) = connected_pair();
    let mut lookup = Lookup { token: 1, endpoint };
    selector.register(lookup.token, &mut lookup.endpoint).unwrap();
    poll_until(&mut selector, &mut lookup, |endpoint| endpoint.writable > 0);
    peer.write_all(b"old connection").unwrap();
    poll_until(&mut selector, &mut lookup, |endpoint| endpoint.readable > 0);
    selector.unregister(lookup.token, &mut lookup.endpoint).unwrap();

    let (endpoint, mut new_peer) = connected_pair();
    lookup = Lookup {
        token: (1 << (usize::BITS / 2)) | 1,
        endpoint,
    };
    selector.register(lookup.token, &mut lookup.endpoint).unwrap();
    poll_until(&mut selector, &mut lookup, |endpoint| endpoint.writable > 0);
    // Old readiness and cancellation completions must not make the replacement readable.
    for _ in 0..10 {
        selector.poll(&mut lookup).unwrap();
    }
    assert_eq!(lookup.endpoint.readable, 0);
    new_peer.write_all(b"new connection").unwrap();
    poll_until(&mut selector, &mut lookup, |endpoint| endpoint.readable > 0);
    selector.unregister(lookup.token, &mut lookup.endpoint).unwrap();
    selector.poll(&mut lookup).unwrap();
}

#[cfg(feature = "mio")]
#[test]
fn mio_uses_active_lookup_across_connection_replacement() {
    connection_replacement(boomnet::service::select::mio::MioSelector::new().unwrap());
}

#[cfg(all(target_os = "linux", feature = "io-uring"))]
#[test]
fn io_uring_uses_active_lookup_across_connection_replacement() {
    connection_replacement(boomnet::service::select::io_uring::IoUringSelector::new().unwrap());
}
