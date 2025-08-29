use smallstr::SmallString;
use smallvec::SmallVec;
use std::fmt::Display;
use std::io;
use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::TryRecvError;
use std::thread::JoinHandle;

const MAX_ADDRS_PER_QUERY: usize = 32;

pub trait DnsResolver {
    type Query: DnsQuery;

    fn new_query(&mut self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query>;
}

pub trait DnsQuery {
    fn poll(&mut self) -> io::Result<impl Iterator<Item = SocketAddr>>;
}

pub struct BlockingDnsResolver;

impl DnsResolver for BlockingDnsResolver {
    type Query = BlockingDnsQuery;

    fn new_query(&mut self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query> {
        Ok(BlockingDnsQuery {
            host: host.as_ref().into(),
            port,
            addrs: None,
        })
    }
}

pub struct BlockingDnsQuery {
    host: SmallString<[u8; 32]>,
    port: u16,
    addrs: Option<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>,
}

impl DnsQuery for BlockingDnsQuery {
    fn poll(&mut self) -> io::Result<impl Iterator<Item = SocketAddr>> {
        let addrs = self.addrs.get_or_insert_with(|| {
            format!("{}:{}", self.host, self.port)
                .to_socket_addrs()
                .unwrap()
                .take(MAX_ADDRS_PER_QUERY)
                .collect()
        });
        Ok(addrs.iter().cloned())
    }
}

pub struct AsyncDnsResolver {
    requests: std::sync::mpsc::SyncSender<DnsRequest>,
    _handle: JoinHandle<()>,
}

impl AsyncDnsResolver {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(256);
        let handle = DnsWorker::start_on_thread(rx);
        AsyncDnsResolver {
            requests: tx,
            _handle: handle,
        }
    }
    // new_with_config (... affinity)
}

impl Default for AsyncDnsResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsResolver for AsyncDnsResolver {
    type Query = AsyncDnsQuery;

    fn new_query(&mut self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let request = DnsRequest {
            response_channel: tx,
            host: host.as_ref().into(),
            port,
        };
        self.requests.try_send(request).map_err(io::Error::other)?;
        Ok(AsyncDnsQuery::mew(rx))
    }
}

pub struct AsyncDnsQuery {
    response: std::sync::mpsc::Receiver<DnsResponse>,
    addrs: Option<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>,
}

impl AsyncDnsQuery {
    fn mew(response: std::sync::mpsc::Receiver<DnsResponse>) -> Self {
        Self { response, addrs: None }
    }
}

impl DnsQuery for AsyncDnsQuery {
    fn poll(&mut self) -> io::Result<impl Iterator<Item = SocketAddr>> {
        if let Some(addrs) = self.addrs.as_ref() {
            let addrs = addrs.clone();
            return Ok(addrs.into_iter());
        }
        match self.response.try_recv() {
            Ok(res) => {
                self.addrs = Some(res.addrs);
                Ok(self.addrs.as_ref().unwrap().clone().into_iter())
            }
            Err(TryRecvError::Empty) => Err(io::Error::new(ErrorKind::WouldBlock, "try again")),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("channel disconnected")),
        }
    }
}

struct DnsWorker {
    requests: std::sync::mpsc::Receiver<DnsRequest>,
}

impl DnsWorker {
    fn start_on_thread(requests: std::sync::mpsc::Receiver<DnsRequest>) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let mut worker = Self { requests };
            loop {
                worker.poll().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    }

    fn poll(&mut self) -> io::Result<()> {
        match self.requests.try_recv() {
            Ok(req) => {
                let addrs = format!("{}:{}", req.host, req.port)
                    .to_socket_addrs()?
                    .take(MAX_ADDRS_PER_QUERY)
                    .collect();
                req.response_channel
                    .try_send(DnsResponse { addrs })
                    .map_err(io::Error::other)?;
                Ok(())
            }
            Err(TryRecvError::Empty) => Ok(()),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("channel disconnected")),
        }
    }
}

struct DnsRequest {
    response_channel: std::sync::mpsc::SyncSender<DnsResponse>,
    host: SmallString<[u8; 32]>,
    port: u16,
}

impl Display for DnsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

struct DnsResponse {
    addrs: SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>,
}

#[cfg(test)]
mod tests {
    use crate::service::dns::{AsyncDnsResolver, BlockingDnsResolver, DnsQuery, DnsResolver};
    use std::io::ErrorKind;

    #[test]
    #[ignore]
    fn should_resolve_blocking() {
        let mut resolver = BlockingDnsResolver;
        let mut query = resolver.new_query("fstream.binance.com", 443).unwrap();
        let addrs = query.poll().unwrap().collect::<Vec<_>>();
        println!("{:#?}", addrs);
    }

    #[test]
    #[ignore]
    fn should_resolve_async() {
        let mut resolver = AsyncDnsResolver::new();
        let mut query = resolver.new_query("fstream.binance.com", 443).unwrap();
        loop {
            match query.poll() {
                Ok(addrs) => {
                    println!("{:#?}", addrs.collect::<Vec<_>>());
                    break;
                }
                Err(err) => if err.kind() == ErrorKind::WouldBlock {},
            }
        }
    }
}
