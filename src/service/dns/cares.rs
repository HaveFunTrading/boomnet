use super::{DnsQuery, DnsResolver, MAX_ADDRS_PER_QUERY, MAX_HOSTNAME_LEN_BEFORE_SPILL};
use polling::{Event, Events, Poller};
use smallstr::SmallString;
use smallvec::SmallVec;
use std::fmt::{Display, Formatter};
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::os::fd::BorrowedFd;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// Caller-driven asynchronous DNS resolver backed by `c-ares`.
///
/// Each query owns its resolver channel and performs non-blocking DNS I/O when
/// [`DnsQuery::poll`] is called. No worker or event-loop thread is created.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaresDnsResolver;

impl CaresDnsResolver {
    /// Create a caller-driven resolver.
    pub const fn new() -> Self {
        Self
    }
}

impl DnsResolver for CaresDnsResolver {
    type Query = CaresDnsQuery;

    fn new_query(&self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query> {
        CaresDnsQuery::new(host, port)
    }
}

type CaresResponse = io::Result<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>;

/// A caller-driven, non-blocking DNS query produced by [`CaresDnsResolver`].
///
/// Calling [`DnsQuery::poll`] checks the DNS sockets without waiting and lets
/// `c-ares` process any ready I/O or expired timeouts.
pub struct CaresDnsQuery {
    channel: c_ares::Channel,
    poller: Poller,
    events: Events,
    response: Receiver<CaresResponse>,
    addrs: Option<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>,
    host: SmallString<[u8; MAX_HOSTNAME_LEN_BEFORE_SPILL]>,
    port: u16,
}

impl CaresDnsQuery {
    fn new(host: impl AsRef<str>, port: u16) -> io::Result<Self> {
        let host: SmallString<[u8; MAX_HOSTNAME_LEN_BEFORE_SPILL]> = host.as_ref().into();
        let mut channel = c_ares::Channel::new().map_err(cares_error)?;
        let poller = Poller::new()?;
        let events = Events::with_capacity(NonZeroUsize::MIN);
        let (response_tx, response) = std::sync::mpsc::sync_channel(1);
        let mut service_buffer = itoa::Buffer::new();
        let service = service_buffer.format(port);
        let hints = c_ares::AddrInfoHints {
            flags: c_ares::AddrInfoFlags::NUMERICSERV,
            ..Default::default()
        };

        channel.get_addrinfo(&host, Some(service), &hints, move |result| {
            let result = result.map_err(cares_error).map(|result| {
                result
                    .nodes()
                    .filter_map(|node| node.socket_addr())
                    .take(MAX_ADDRS_PER_QUERY)
                    .collect()
            });
            let _ = response_tx.try_send(result);
        });

        Ok(Self {
            channel,
            poller,
            events,
            response,
            addrs: None,
            host,
            port,
        })
    }

    fn try_response(&mut self) -> io::Result<Option<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>> {
        match self.response.try_recv() {
            Ok(Ok(addrs)) => Ok(Some(addrs)),
            Ok(Err(err)) => Err(err),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("c-ares callback disconnected")),
        }
    }
}

impl Display for CaresDnsQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl DnsQuery for CaresDnsQuery {
    fn poll(&mut self) -> io::Result<impl IntoIterator<Item = SocketAddr>> {
        if let Some(addrs) = self.addrs.as_ref() {
            return Ok(addrs.clone());
        }
        if let Some(addrs) = self.try_response()? {
            self.addrs = Some(addrs);
            return Ok(self.addrs.as_ref().unwrap().clone());
        }

        poll_cares_channel(&mut self.channel, &self.poller, &mut self.events)?;
        match self.try_response()? {
            Some(addrs) => {
                self.addrs = Some(addrs);
                Ok(self.addrs.as_ref().unwrap().clone())
            }
            None => Err(ErrorKind::WouldBlock.into()),
        }
    }
}

fn poll_cares_channel(channel: &mut c_ares::Channel, poller: &Poller, events: &mut Events) -> io::Result<()> {
    let sockets = channel.sockets();
    let mut registered = 0;
    for (socket, readable, writable) in sockets.iter() {
        let key = match socket_key(socket) {
            Ok(key) => key,
            Err(err) => {
                let _ = delete_sockets(poller, sockets.iter().take(registered));
                return Err(err);
            }
        };
        let event = Event::new(key, readable, writable);
        // SAFETY: c-ares owns the socket. It is removed from the poller before
        // c-ares is allowed to process and potentially close it.
        if let Err(err) = unsafe { poller.add(socket, event) } {
            let _ = delete_sockets(poller, sockets.iter().take(registered));
            return Err(err);
        }
        registered += 1;
    }

    if registered == 0 {
        channel.process_fd(None, None);
        return Ok(());
    }

    events.clear();
    let ready = match poller.wait(events, Some(Duration::ZERO)) {
        Ok(_) => events
            .iter()
            .next()
            .map(|event| {
                c_ares::Socket::try_from(event.key)
                    .map(|socket| (socket, event.readable, event.writable))
                    .map_err(|_| io::Error::other("c-ares socket does not fit its platform type"))
            })
            .transpose(),
        Err(err) => Err(err),
    };

    let delete_result = delete_sockets(poller, sockets.iter());
    let ready = ready?;
    delete_result?;

    if let Some((socket, readable, writable)) = ready {
        // Processing one socket can complete the query and close the others.
        // Any additional readiness will be rediscovered by the next call.
        channel.process_fd(readable.then_some(socket), writable.then_some(socket));
    } else {
        channel.process_fd(None, None);
    }
    Ok(())
}

fn delete_sockets(poller: &Poller, sockets: impl Iterator<Item = (c_ares::Socket, bool, bool)>) -> io::Result<()> {
    let mut first_error = None;
    for (socket, _, _) in sockets {
        let source = unsafe { borrow_socket(socket) };
        if let Err(err) = poller.delete(source)
            && first_error.is_none()
        {
            first_error = Some(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

fn socket_key(socket: c_ares::Socket) -> io::Result<usize> {
    usize::try_from(socket).map_err(|_| io::Error::other("c-ares returned an invalid socket"))
}

unsafe fn borrow_socket(socket: c_ares::Socket) -> impl polling::AsSource {
    unsafe { BorrowedFd::borrow_raw(socket) }
}

fn cares_error(err: c_ares::Error) -> io::Error {
    let kind = match err {
        c_ares::Error::ENODATA | c_ares::Error::ENOTFOUND | c_ares::Error::ENONAME => ErrorKind::NotFound,
        c_ares::Error::ETIMEOUT => ErrorKind::TimedOut,
        c_ares::Error::ECONNREFUSED => ErrorKind::ConnectionRefused,
        _ => ErrorKind::Other,
    };
    io::Error::new(kind, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_resolve_numeric_address() {
        let resolver = CaresDnsResolver::new();
        let mut query = resolver.new_query("127.0.0.1", 443).unwrap();
        let addrs = query.poll().unwrap().into_iter().collect::<Vec<_>>();
        assert_eq!(addrs, ["127.0.0.1:443".parse().unwrap()]);
    }

    #[test]
    #[ignore]
    fn should_resolve_dns() {
        let resolver = CaresDnsResolver::new();
        let mut query = resolver.new_query("example.com", 443).unwrap();
        loop {
            match query.poll() {
                Ok(addrs) => {
                    assert!(addrs.into_iter().all(|addr| addr.port() == 443));
                    break;
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("DNS query failed: {err}"),
            }
        }
    }
}
