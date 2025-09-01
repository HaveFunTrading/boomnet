//! Minimal DNS resolving abstractions with both blocking and async implementations.
//!
//! ## Examples
//! Blocking resolver.
//!```no_run
//! use std::io;
//! use boomnet::service::dns::{DnsQuery, DnsResolver, BlockingDnsResolver};
//!
//! fn main() -> io::Result<()> {
//!     let mut r = BlockingDnsResolver;
//!     let mut q = r.new_query("example.com", 80)?;
//!     let addrs = q.poll()?; // resolves on first call
//!     for addr in addrs { println!("{addr}"); }
//!     Ok(())
//! }
//! ```
//!
//! Asynchronous resolver. Will perform resolution on a background thread.
//!```no_run
//! use std::io::{self, ErrorKind};
//! use boomnet::service::dns::{DnsQuery, DnsResolver, AsyncDnsResolver};
//!
//! fn main() -> io::Result<()> {
//!     let mut r = AsyncDnsResolver::new()?;
//!     let mut q = r.new_query("example.com", 80)?;
//!     loop {
//!         match q.poll() {
//!             Ok(addrs) => { for a in addrs { println!("{a}"); } break; }
//!             Err(e) if e.kind() == ErrorKind::WouldBlock => { /* try again later */ }
//!             Err(e) => return Err(e),
//!         }
//!     }
//!     Ok(())
//! }
//! ```

use core_affinity::CoreId;
use smallstr::SmallString;
use smallvec::SmallVec;
use std::fmt::Display;
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::TryRecvError;
use std::thread::JoinHandle;
use std::{io, thread};

const MAX_ADDRS_PER_QUERY: usize = 32;
const MAX_HOSTNAME_LEN_BEFORE_SPILL: usize = 64;

/// A resolver capable of creating DNS queries.
///
/// Implementors return a [`DnsQuery`] that can be polled for results.
/// The `host` is a UTF-8 hostname (no port), and `port` is appended
/// to form the socket addresses.
pub trait DnsResolver {
    /// The concrete query type produced by this resolver.
    type Query: DnsQuery;

    /// Start a new DNS lookup for `host:port`.
    fn new_query(&self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query>;
}

/// A DNS query that yields one or more `SocketAddr`s.
///
/// Returns an iterator of resolved addresses when ready.
pub trait DnsQuery {
    /// Try to obtain resolved addresses. If `Err(WouldBlock)` is returned it means the result
    /// is not ready and the user should call `poll` again.
    fn poll(&mut self) -> io::Result<impl IntoIterator<Item = SocketAddr>>;
}

/// Blocking DNS resolver.
pub struct BlockingDnsResolver;

impl DnsResolver for BlockingDnsResolver {
    type Query = BlockingDnsQuery;

    fn new_query(&self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query> {
        Ok(BlockingDnsQuery {
            host: host.as_ref().into(),
            port,
            addrs: None,
        })
    }
}

/// A blocking DNS query.
pub struct BlockingDnsQuery {
    host: SmallString<[u8; MAX_HOSTNAME_LEN_BEFORE_SPILL]>,
    port: u16,
    addrs: Option<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>,
}

impl DnsQuery for BlockingDnsQuery {
    fn poll(&mut self) -> io::Result<impl IntoIterator<Item = SocketAddr>> {
        let addrs = self.addrs.get_or_insert_with(|| {
            (&*self.host, self.port)
                .to_socket_addrs()
                .unwrap()
                .take(MAX_ADDRS_PER_QUERY)
                .collect()
        });
        Ok(addrs.clone())
    }
}

/// No CPU affinity for the async worker (type-state marker).
pub struct NoAffinity;
/// Select CPU by index from the available core set (type-state marker).
pub struct AffinityCpuIndex;
/// Select CPU by explicit `CoreId` (type-state marker).
pub struct AffinityCpuId;

/// Affinity behavior for the async worker thread.
///
/// Implemented by the provided marker types. Users typically configure
/// affinity through [`AsyncDnsResolverConfig`] helpers instead of
/// implementing this trait directly.
pub trait AffinityConfig {
    fn get_core_id<S>(cfg: &AsyncDnsResolverConfig<S>, cpu_set: Vec<CoreId>) -> Option<CoreId>;
}

/// Configuration for [`AsyncDnsResolver`] using type-state to guide CPU affinity.
#[derive(Debug)]
pub struct AsyncDnsResolverConfig<S> {
    affinity_cpu_index: Option<usize>,
    affinity_cpu_id: Option<CoreId>,
    state: PhantomData<S>,
}

impl AsyncDnsResolverConfig<NoAffinity> {
    /// Create a config with no CPU affinity.
    pub fn new() -> AsyncDnsResolverConfig<NoAffinity> {
        AsyncDnsResolverConfig {
            affinity_cpu_index: None,
            affinity_cpu_id: None,
            state: PhantomData,
        }
    }
}

impl Default for AsyncDnsResolverConfig<NoAffinity> {
    fn default() -> AsyncDnsResolverConfig<NoAffinity> {
        AsyncDnsResolverConfig::new()
    }
}

impl<S: AffinityConfig> AsyncDnsResolverConfig<S> {
    /// Resolve the worker's `CoreId` from a discovered CPU set using the selected affinity policy.
    pub fn get_core_id(&self, cpu_set: Vec<CoreId>) -> Option<CoreId> {
        S::get_core_id(self, cpu_set)
    }
}

impl AsyncDnsResolverConfig<NoAffinity> {
    /// Pin the async worker to the `cpu_index`-th core.
    pub fn with_cpu_index(self, cpu_index: usize) -> AsyncDnsResolverConfig<AffinityCpuIndex> {
        AsyncDnsResolverConfig {
            affinity_cpu_index: Some(cpu_index),
            affinity_cpu_id: None,
            state: PhantomData,
        }
    }

    /// Pin the async worker to a specific CPU by numeric id.
    pub fn with_cpu_id(self, cpu_id: usize) -> AsyncDnsResolverConfig<AffinityCpuId> {
        AsyncDnsResolverConfig {
            affinity_cpu_index: None,
            affinity_cpu_id: Some(CoreId { id: cpu_id }),
            state: PhantomData,
        }
    }
}

impl AffinityConfig for NoAffinity {
    fn get_core_id<S>(_cfg: &AsyncDnsResolverConfig<S>, _cpu_set: Vec<CoreId>) -> Option<CoreId> {
        None
    }
}

impl AffinityConfig for AffinityCpuId {
    fn get_core_id<S>(cfg: &AsyncDnsResolverConfig<S>, cpu_set: Vec<CoreId>) -> Option<CoreId> {
        assert!(cpu_set.contains(cfg.affinity_cpu_id.as_ref()?), "core id not present in the available cpu set");
        cfg.affinity_cpu_id
    }
}

impl AffinityConfig for AffinityCpuIndex {
    fn get_core_id<S>(cfg: &AsyncDnsResolverConfig<S>, cpu_set: Vec<CoreId>) -> Option<CoreId> {
        Some(cpu_set[cfg.affinity_cpu_index?])
    }
}

/// Async DNS resolver with an internal worker thread.
///
/// The worker optionally pins to a chosen CPU core (see [`AsyncDnsResolverConfig`]).
/// Queries are non-blocking: call `poll()` until results are available.
pub struct AsyncDnsResolver {
    requests: std::sync::mpsc::SyncSender<DnsRequest>,
    _handle: JoinHandle<()>,
}

impl AsyncDnsResolver {
    /// Create an async resolver with default configuration (no CPU affinity).
    pub fn new() -> io::Result<Self> {
        Self::new_with_config(Default::default())
    }

    /// Create an async resolver using the provided configuration.
    pub fn new_with_config<S: AffinityConfig>(cfg: AsyncDnsResolverConfig<S>) -> io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(256);
        let cpu_set =
            core_affinity::get_core_ids().ok_or_else(|| io::Error::other("unable to retrieve available cpu set"))?;
        let core_id = cfg.get_core_id(cpu_set);
        let handle = DnsWorker::start_on_thread(rx, core_id)?;
        Ok(AsyncDnsResolver {
            requests: tx,
            _handle: handle,
        })
    }
}

impl DnsResolver for AsyncDnsResolver {
    type Query = AsyncDnsQuery;

    fn new_query(&self, host: impl AsRef<str>, port: u16) -> io::Result<Self::Query> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let request = DnsRequest {
            response_channel: tx,
            host: host.as_ref().into(),
            port,
        };
        self.requests.try_send(request).map_err(io::Error::other)?;
        Ok(AsyncDnsQuery::new(rx))
    }
}

/// A non-blocking DNS query produced by [`AsyncDnsResolver`].
///
/// Use [`DnsQuery::poll`] repeatedly; it returns `Err(WouldBlock)` until results are ready.
pub struct AsyncDnsQuery {
    response: std::sync::mpsc::Receiver<DnsResponse>,
    addrs: Option<SmallVec<[SocketAddr; MAX_ADDRS_PER_QUERY]>>,
}

impl AsyncDnsQuery {
    fn new(response: std::sync::mpsc::Receiver<DnsResponse>) -> Self {
        Self { response, addrs: None }
    }
}

impl DnsQuery for AsyncDnsQuery {
    fn poll(&mut self) -> io::Result<impl IntoIterator<Item = SocketAddr>> {
        if let Some(addrs) = self.addrs.as_ref() {
            let addrs = addrs.clone();
            return Ok(addrs);
        }
        match self.response.try_recv() {
            Ok(res) => {
                self.addrs = Some(res.addrs);
                Ok(self.addrs.as_ref().unwrap().clone())
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
    fn start_on_thread(
        requests: std::sync::mpsc::Receiver<DnsRequest>,
        core_id: Option<CoreId>,
    ) -> io::Result<JoinHandle<()>> {
        let builder = thread::Builder::new().name("dns-worker".to_owned());
        builder.spawn(move || {
            if let Some(core_id) = core_id {
                core_affinity::set_for_current(core_id);
            }
            let mut worker = Self { requests };
            loop {
                match worker.poll() {
                    Ok(_) => {}
                    Err(err) => panic!("dns worker error: {}", err),
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    }

    fn poll(&mut self) -> io::Result<()> {
        match self.requests.try_recv() {
            Ok(req) => {
                let addrs = (&*req.host, req.port)
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
    host: SmallString<[u8; MAX_HOSTNAME_LEN_BEFORE_SPILL]>,
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
        let resolver = BlockingDnsResolver;
        let mut query = resolver.new_query("fstream.binance.com", 443).unwrap();
        let addrs = query.poll().unwrap().into_iter().collect::<Vec<_>>();
        println!("{:#?}", addrs);
    }

    #[test]
    #[ignore]
    fn should_resolve_async() {
        let resolver = AsyncDnsResolver::new().unwrap();
        let mut query = resolver.new_query("fstream.binance.com", 443).unwrap();
        loop {
            match query.poll() {
                Ok(addrs) => {
                    println!("{:#?}", addrs.into_iter().collect::<Vec<_>>());
                    break;
                }
                Err(err) => if err.kind() == ErrorKind::WouldBlock {},
            }
        }
    }
}
