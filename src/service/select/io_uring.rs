//! Linux `io_uring` readiness selector.
//!
//! This backend uses `IORING_OP_POLL_ADD` for readiness only. Socket reads remain synchronous,
//! allowing TLS implementations to read directly into their existing buffers without an extra
//! ciphertext copy.

use crate::service::dns::BlockingDnsResolver;
use crate::service::endpoint::{Context, Endpoint, EndpointWithContext};
use crate::service::node::IONode;
use crate::service::select::{Selectable, Selector, SelectorToken};
use crate::service::time::SystemTimeClockSource;
use crate::service::{IOService, IntoIOService, IntoIOServiceWithContext};
use io_uring::{IoUring, cqueue, opcode, squeue, types};
use std::collections::HashMap;
use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

/// Configuration for [`IoUringSelector`].
#[derive(Debug, Clone, Copy)]
pub struct IoUringConfig {
    /// Submission and completion ring capacity.
    pub entries: u32,
    /// Maximum time `poll` may wait for a completion. `None` makes polling nonblocking.
    pub wait_timeout: Option<Duration>,
    /// Ring-level NAPI busy-poll timeout in microseconds. `None` disables ring NAPI setup.
    pub napi_busy_poll_timeout: Option<u32>,
    /// Prefer NAPI busy polling over device interrupts. Enabling this requires `CAP_NET_ADMIN`.
    pub prefer_busy_poll: bool,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            entries: 64,
            wait_timeout: None,
            napi_busy_poll_timeout: None,
            prefer_busy_poll: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum Operation {
    Connect = 1,
    Read = 2,
    Cancel = 3,
}

impl Operation {
    fn from_user_data(user_data: u64) -> Option<Self> {
        match (user_data >> 32) as u8 {
            1 => Some(Self::Connect),
            2 => Some(Self::Read),
            3 => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Registration {
    fd: RawFd,
    operation: Operation,
}

fn user_data(token: SelectorToken, operation: Operation) -> u64 {
    ((operation as u64) << 32) | u64::from(token)
}

fn token_from_user_data(user_data: u64) -> SelectorToken {
    user_data as SelectorToken
}

/// Readiness-only `io_uring` selector.
pub struct IoUringSelector<S> {
    ring: IoUring,
    config: IoUringConfig,
    registrations: HashMap<SelectorToken, Registration>,
    completions: Vec<(u64, i32, u32)>,
    rearms: Vec<(SelectorToken, RawFd, Operation)>,
    next_token: SelectorToken,
    phantom: PhantomData<S>,
}

impl<S> IoUringSelector<S> {
    /// Create a fully nonblocking selector with the default ring capacity.
    ///
    /// This does not wait for completions and does not register ring-level NAPI busy polling.
    pub fn new() -> io::Result<Self> {
        Self::new_with_config(IoUringConfig::default())
    }

    /// Create a selector with explicit waiting and NAPI behavior.
    pub fn new_with_config(config: IoUringConfig) -> io::Result<Self> {
        if config.entries == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "io_uring entries must be non-zero"));
        }

        let mut ring = IoUring::builder().setup_single_issuer().build(config.entries)?;
        if let Some(timeout) = config.napi_busy_poll_timeout {
            let mut napi = types::Napi::new()
                .set_busy_poll_timeout(timeout)
                .set_prefer_busy_poll(config.prefer_busy_poll);
            ring.submitter().register_napi(&mut napi)?;
        }
        let completion_capacity = ring.completion().capacity();

        Ok(Self {
            ring,
            config,
            registrations: HashMap::new(),
            completions: Vec::with_capacity(completion_capacity),
            rearms: Vec::with_capacity(completion_capacity),
            next_token: 0,
            phantom: PhantomData,
        })
    }

    fn push(&mut self, entry: squeue::Entry) -> io::Result<()> {
        let pushed = {
            let mut submission = self.ring.submission();
            // SAFETY: the entry owns no userspace pointers and all referenced file descriptors
            // remain owned by registered IO nodes until their poll requests are cancelled.
            unsafe { submission.push(&entry).is_ok() }
        };
        if pushed {
            return Ok(());
        }

        self.ring.submit()?;
        let mut submission = self.ring.submission();
        // SAFETY: same reasoning as above; submitting made room in the bounded SQ.
        unsafe { submission.push(&entry) }.map_err(|_| io::Error::other("io_uring submission queue is full"))
    }

    fn submit_pending(&mut self) -> io::Result<usize> {
        if self.ring.submission().is_empty() {
            Ok(0)
        } else {
            self.ring.submit()
        }
    }

    fn arm(&mut self, token: SelectorToken, fd: RawFd, operation: Operation) -> io::Result<()> {
        let flags = match operation {
            Operation::Connect => libc::POLLOUT | libc::POLLERR | libc::POLLHUP,
            Operation::Read => libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            Operation::Cancel => unreachable!(),
        } as u32;
        let entry = opcode::PollAdd::new(types::Fd(fd), flags)
            .multi(operation == Operation::Read)
            .build()
            .user_data(user_data(token, operation));
        self.push(entry)
    }

    fn wait(&mut self) -> io::Result<usize> {
        if self.registrations.is_empty() {
            return self.submit_pending();
        }
        let Some(timeout) = self.config.wait_timeout else {
            return self.submit_pending();
        };
        if timeout.is_zero() {
            return self.submit_pending();
        }

        let timespec = types::Timespec::from(timeout);
        let args = types::SubmitArgs::new().timespec(&timespec);
        match self.ring.submitter().submit_with_args(1, &args) {
            Err(error) if error.raw_os_error() == Some(libc::ETIME) => Ok(0),
            result => result,
        }
    }

    fn collect_completions(&mut self) {
        self.completions.clear();
        let mut queue = self.ring.completion();
        for completion in &mut queue {
            self.completions
                .push((completion.user_data(), completion.result(), completion.flags()));
        }
    }
}

impl<S: AsRawFd + Selectable> Selector for IoUringSelector<S> {
    type Target = S;

    fn register<E>(&mut self, token: SelectorToken, io_node: &mut IONode<Self::Target, E>) -> io::Result<()> {
        let fd = io_node.as_stream().as_raw_fd();
        self.arm(token, fd, Operation::Connect)?;
        self.ring.submit()?;
        self.registrations.insert(
            token,
            Registration {
                fd,
                operation: Operation::Connect,
            },
        );
        Ok(())
    }

    fn unregister<E>(&mut self, io_node: &mut IONode<Self::Target, E>) -> io::Result<()> {
        let fd = io_node.as_stream().as_raw_fd();
        let Some(token) = self
            .registrations
            .iter()
            .find_map(|(token, registration)| (registration.fd == fd).then_some(*token))
        else {
            return Ok(());
        };
        let registration = self.registrations.remove(&token).unwrap();
        let entry = opcode::PollRemove::new(user_data(token, registration.operation))
            .build()
            .user_data(user_data(token, Operation::Cancel));
        self.push(entry)?;
        self.ring.submit()?;
        Ok(())
    }

    fn poll<E>(&mut self, io_nodes: &mut HashMap<SelectorToken, IONode<Self::Target, E>>) -> io::Result<()> {
        self.wait()?;
        self.collect_completions();
        self.rearms.clear();

        for index in 0..self.completions.len() {
            let (data, result, flags) = self.completions[index];
            let Some(operation) = Operation::from_user_data(data) else {
                continue;
            };
            if operation == Operation::Cancel {
                continue;
            }
            let token = token_from_user_data(data);
            let Some(registration) = self.registrations.get(&token).copied() else {
                continue;
            };
            if registration.operation != operation {
                continue;
            }
            if result < 0 {
                let errno = -result;
                if matches!(errno, libc::ECANCELED | libc::ENOENT) {
                    continue;
                }
                return Err(io::Error::from_raw_os_error(errno));
            }

            let Some(io_node) = io_nodes.get_mut(&token) else {
                continue;
            };
            match operation {
                Operation::Connect => {
                    if io_node.as_stream_mut().connected()? {
                        io_node.as_stream_mut().make_writable()?;
                        self.registrations.get_mut(&token).unwrap().operation = Operation::Read;
                        self.rearms.push((token, registration.fd, Operation::Read));
                    } else {
                        self.rearms.push((token, registration.fd, Operation::Connect));
                    }
                }
                Operation::Read => {
                    io_node.as_stream_mut().make_readable()?;
                    if !cqueue::more(flags) {
                        self.rearms.push((token, registration.fd, Operation::Read));
                    }
                }
                Operation::Cancel => unreachable!(),
            }
        }

        for index in 0..self.rearms.len() {
            let (token, fd, operation) = self.rearms[index];
            if self.registrations.contains_key(&token) {
                self.arm(token, fd, operation)?;
            }
        }
        self.submit_pending()?;
        Ok(())
    }

    #[inline]
    fn next_token(&mut self) -> SelectorToken {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        token
    }
}

impl<E: Endpoint> IntoIOService<E> for IoUringSelector<E::Target>
where
    E::Target: AsRawFd + Selectable,
{
    fn into_io_service(self) -> IOService<Self, E, (), SystemTimeClockSource, BlockingDnsResolver> {
        IOService::new(self, SystemTimeClockSource, BlockingDnsResolver)
    }
}

impl<C: Context, E: EndpointWithContext<C>> IntoIOServiceWithContext<E, C> for IoUringSelector<E::Target>
where
    E::Target: AsRawFd + Selectable,
{
    fn into_io_service_with_context(self) -> IOService<Self, E, C, SystemTimeClockSource, BlockingDnsResolver> {
        IOService::new(self, SystemTimeClockSource, BlockingDnsResolver)
    }
}
