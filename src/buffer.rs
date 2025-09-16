//! Fixed length buffer for reading data from the network.
//!
//! The buffer should be used when implementing protocols on top of streams. It offers
//! a number of methods to retrieve the bytes with zero-copy semantics.

use crate::util::NoBlock;
use std::io::Read;
use std::{io, ptr};

// re-export
pub use pool::*;

const DEFAULT_INITIAL_CAPACITY: usize = 32768;

#[derive(Debug)]
pub struct ReadBuffer<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize = DEFAULT_INITIAL_CAPACITY> {
    inner: Vec<u8>,
    head: usize,
    tail: usize,
}

/// Reading mode that controls [ReadBuffer::read_from] data limit.
enum ReadMode {
    /// Try to read up to one chunk of data.
    Chunk,
    /// Try to read all available data up to the buffer capacity.
    Available,
}

impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> Default for ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
    pub fn new() -> ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
        assert!(
            CHUNK_SIZE <= INITIAL_CAPACITY,
            "CHUNK_SIZE ({CHUNK_SIZE}) must be less or equal than {INITIAL_CAPACITY}"
        );
        Self {
            inner: vec![0u8; INITIAL_CAPACITY],
            head: 0,
            tail: 0,
        }
    }

    #[inline]
    pub const fn empty() -> Self {
        Self {
            inner: Vec::new(),
            head: 0,
            tail: 0,
        }
    }

    #[inline]
    pub fn from_bytes(bytes: Vec<u8>) -> ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
        assert!(
            CHUNK_SIZE <= INITIAL_CAPACITY,
            "CHUNK_SIZE ({CHUNK_SIZE}) must be less or equal than {INITIAL_CAPACITY}"
        );
        assert!(bytes.len() >= INITIAL_CAPACITY, "bytes len must be equal or greater than {INITIAL_CAPACITY}");
        ReadBuffer {
            inner: bytes,
            head: 0,
            tail: 0,
        }
    }

    #[inline]
    pub fn into_bytes(self) -> Vec<u8> {
        self.inner
    }

    #[inline]
    pub const fn available(&self) -> usize {
        self.tail - self.head
    }

    /// Reads up to `CHUNK_SIZE` into buffer from the provided `stream`. If there is no more space
    /// available to accommodate the next read of up to chunk size, the buffer will grow by a factor of 2.
    #[inline]
    pub fn read_from<S: Read>(&mut self, stream: &mut S) -> io::Result<()> {
        self.read_from_with_mode(stream, ReadMode::Chunk)
    }

    /// Reads all available bytes into buffer from the provided `stream`. If there is no more space
    /// available to accommodate the next read of up to `CHUNK_SIZE`, the buffer will grow by a factor of 2.
    /// This method is usually preferred to [`ReadBuffer::read_from`] as it takes advantage of all available
    /// space in the buffer therefore reducing the number of operating system calls and increasing the throughput.
    #[inline]
    pub fn read_all_from<S: Read>(&mut self, stream: &mut S) -> io::Result<()> {
        self.read_from_with_mode(stream, ReadMode::Available)
    }

    #[inline]
    fn read_from_with_mode<S: Read>(&mut self, stream: &mut S, read_mode: ReadMode) -> io::Result<()> {
        #[cold]
        fn grow(buf: &mut Vec<u8>) {
            buf.resize(buf.len() * 2, 0u8);
        }

        #[cold]
        fn compact<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize>(
            buf: &mut ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>,
        ) {
            unsafe { ptr::copy(buf.inner.as_ptr().add(buf.head), buf.inner.as_mut_ptr(), buf.available()) }
            buf.tail -= buf.head;
            buf.head = 0;
        }

        // compact
        if self.head > 0 && self.available() > 0 {
            compact(self);
        }

        // clear
        if self.head > 0 && self.available() == 0 {
            self.head = 0;
            self.tail = 0;
        }

        // ensure capacity for at least one chunk
        if self.tail + CHUNK_SIZE > self.inner.capacity() {
            grow(&mut self.inner);
        }

        let read = match read_mode {
            ReadMode::Chunk => stream.read(&mut self.inner[self.tail..self.tail + CHUNK_SIZE]),
            ReadMode::Available => stream.read(&mut self.inner[self.tail..]),
        };

        self.tail += read.no_block()?;
        Ok(())
    }

    #[inline]
    pub fn consume_next(&mut self, len: usize) -> Option<&'static [u8]> {
        match self.available() >= len {
            true => Some(unsafe { self.consume_next_unchecked(len) }),
            false => None,
        }
    }

    /// # Safety
    /// This function should only be called after `available` bytes are known.
    /// ```no_run
    /// use boomnet::buffer::ReadBuffer;
    ///
    /// let mut buffer = ReadBuffer::<4096>::new();
    /// if buffer.available() > 10 {
    ///     unsafe {
    ///         let view = buffer.consume_next_unchecked(10);
    ///     }
    /// }
    #[inline]
    pub unsafe fn consume_next_unchecked(&mut self, len: usize) -> &'static [u8] {
        unsafe {
            let consumed_view = &*ptr::slice_from_raw_parts(self.inner.as_ptr().add(self.head), len);
            self.head += len;
            consumed_view
        }
    }

    #[inline]
    pub fn consume_next_byte(&mut self) -> Option<u8> {
        match self.available() >= 1 {
            true => Some(unsafe { self.consume_next_byte_unchecked() }),
            false => None,
        }
    }

    /// # Safety
    /// This function should only be called after `available` bytes are known.
    /// ```no_run
    /// use boomnet::buffer::ReadBuffer;
    ///
    /// let mut buffer = ReadBuffer::<4096>::new();
    /// if buffer.available() > 0 {
    ///     unsafe {
    ///         let byte = buffer.consume_next_byte_unchecked();
    ///     }
    /// }
    #[inline]
    pub unsafe fn consume_next_byte_unchecked(&mut self) -> u8 {
        unsafe {
            let byte = *self.inner.as_ptr().add(self.head);
            self.head += 1;
            byte
        }
    }

    #[inline]
    pub fn view(&self) -> &[u8] {
        &self.inner[self.head..self.tail]
    }

    #[inline]
    pub fn view_last(&self, len: usize) -> &[u8] {
        &self.inner[self.tail - len..self.tail]
    }
}

/// Pooled, per-thread reusable byte buffers.
///
/// This module exposes a **per-thread** buffer pool. Call
/// [`default_buffer_pool_ref`] to obtain a cheap, clonable handle to the
/// current thread’s pool. Acquired buffers are returned to the pool on
/// [`Drop`] via an RAII guard ([`OwnedReadBuffer`]).
///
/// ## Concurrency
/// - The pool is **thread-local**. Each thread gets its own pool instance.
/// - Handles are built on `Rc<RefCell<…>>` and are therefore **not** `Send`/`Sync`.
///
/// ## Complexity notes
/// - `acquire` performs a linear scan to find a buffer with `len() >= INITIAL_CAPACITY`.
///   This is O(n) in the number of stored buffers.
///
/// ## Example
/// ```no_run
/// // Get this thread's pool and acquire a buffer.
/// use boomnet::buffer::default_buffer_pool_ref;
///
/// let mut pool = default_buffer_pool_ref();
/// let mut buf = pool.acquire::<4096, 8192>();
///
/// // Use the buffer (implements `Deref`/`DerefMut` to `ReadBuffer`).
/// // ...
///
/// // On drop, the buffer is automatically returned to the pool.
/// drop(buf);
///
/// // Cloning the handle is cheap (Rc clone); it does NOT create a new pool.
/// let _pool2 = pool.clone();
/// ```
mod pool {
    use crate::buffer::{DEFAULT_INITIAL_CAPACITY, ReadBuffer};
    use std::cell::{OnceCell, RefCell};
    use std::ops::{Deref, DerefMut};
    use std::rc::Rc;

    thread_local! {
        /// Per-thread storage for the default buffer pool handle.
        ///
        /// The value inside is initialized lazily on the first call to
        /// [`default_buffer_pool_ref`] on a given thread.
        static DEFAULT_BUFFER_POOL: OnceCell<BufferPoolRef> = const { OnceCell::new() };
    }

    /// Returns this thread’s default [`BufferPoolRef`] (creating it on first use).
    ///
    /// Cloning the returned handle is cheap (it only clones an `Rc`) and all
    /// clones on the same thread refer to the **same** underlying pool.
    pub fn default_buffer_pool_ref() -> BufferPoolRef {
        DEFAULT_BUFFER_POOL.with(|cell| cell.get_or_init(BufferPoolRef::default).clone())
    }

    /// Cheap, clonable handle to a **per-thread** buffer pool.
    ///
    /// Use [`BufferPoolRef::acquire`] to get an [`OwnedReadBuffer`]; when the
    /// guard is dropped, the buffer is automatically returned to the pool.
    #[derive(Clone, Default, Debug)]
    pub struct BufferPoolRef {
        inner: Rc<RefCell<BufferPool>>,
    }

    impl BufferPoolRef {
        /// Acquire a buffer from the pool (or allocate a new one) and wrap it in an
        /// RAII guard that returns the buffer on [`Drop`].
        pub fn acquire<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize>(
            &self,
        ) -> OwnedReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
            OwnedReadBuffer {
                inner: self.inner.borrow_mut().acquire(),
                pool: self.clone(),
            }
        }

        /// Return a buffer to the pool.
        ///
        /// You typically don’t need to call this directly. Dropping
        /// [`OwnedReadBuffer`] does it for you.
        pub fn release<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize>(
            &self,
            buffer: ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>,
        ) {
            self.inner.borrow_mut().release(buffer)
        }
    }

    /// RAII guard for an acquired pooled buffer.
    ///
    /// When the guard is dropped, the underlying buffer is returned to
    /// the current thread’s pool it came from.
    #[derive(Debug)]
    pub struct OwnedReadBuffer<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize = DEFAULT_INITIAL_CAPACITY> {
        inner: ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>,
        pool: BufferPoolRef,
    }

    impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> Deref for OwnedReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
        type Target = ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> DerefMut
        for OwnedReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>
    {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> Drop for OwnedReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
        fn drop(&mut self) {
            let buffer = std::mem::replace(&mut self.inner, ReadBuffer::empty());
            self.pool.release(buffer)
        }
    }

    /// Simple vector-backed buffer pool.
    ///
    /// Stores raw `Vec<u8>` buffers and hands them out wrapped as `ReadBuffer`.
    /// On `release`, buffers are pushed back for reuse.
    #[derive(Default, Debug)]
    pub struct BufferPool {
        buffers: Vec<Vec<u8>>,
    }

    impl BufferPool {
        /// Acquire a buffer with at least `INITIAL_CAPACITY` bytes.
        ///
        /// Performs a linear scan for the first stored buffer satisfying the
        /// capacity requirement; otherwise allocates a new zeroed vector.
        pub fn acquire<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize>(
            &mut self,
        ) -> ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
            let idx = self.buffers.iter().position(|b| b.capacity() >= INITIAL_CAPACITY);
            let bytes = match idx {
                Some(i) => self.buffers.swap_remove(i),
                None => vec![0u8; INITIAL_CAPACITY],
            };
            ReadBuffer::from_bytes(bytes)
        }

        /// Return a buffer to the pool for future reuse.
        pub fn release<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize>(
            &mut self,
            buffer: ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>,
        ) {
            let bytes = buffer.into_bytes();
            self.buffers.push(bytes);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn should_clone_buffer_ref_without_new_allocation() {
            let a = default_buffer_pool_ref();
            let b = default_buffer_pool_ref();
            assert!(Rc::ptr_eq(&a.inner, &b.inner)); // same allocation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::io::ErrorKind::{UnexpectedEof, WouldBlock};

    #[test]
    fn should_read_from_stream() {
        let mut buf = ReadBuffer::<16>::new();
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
        assert_eq!(0, buf.head);
        assert_eq!(0, buf.tail);

        let mut stream = Cursor::new(b"hello world!");
        buf.read_from(&mut stream).expect("unable to read from the stream");

        assert_eq!(12, buf.available());
        assert_eq!(b"hello world!", buf.view());

        assert_eq!(b"hello ", buf.consume_next(6).unwrap());
        assert_eq!(6, buf.available());
        assert_eq!(b"world!", buf.view());

        assert_eq!(b"world!", buf.consume_next(6).unwrap());
        assert_eq!(0, buf.available());
        assert_eq!(b"", buf.view());

        assert_eq!(12, buf.head, "head");
        assert_eq!(12, buf.tail, "tail");
        assert_eq!(0, buf.available());

        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
    }

    #[test]
    fn should_read_all_from_stream() {
        let mut buf = ReadBuffer::<8>::new();
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
        assert_eq!(0, buf.head);
        assert_eq!(0, buf.tail);

        let mut stream = Cursor::new(b"hello world!");
        buf.read_all_from(&mut stream).expect("unable to read from the stream");

        assert_eq!(12, buf.available());
        assert_eq!(b"hello world!", buf.view());
    }

    #[test]
    fn should_append_on_multiple_read() {
        let mut buf = ReadBuffer::<6>::new();
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());

        let mut stream = Cursor::new(b"hello world!");

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"hello ", buf.view());

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"hello world!", buf.view());

        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
    }

    #[test]
    fn should_clear_on_multiple_read() {
        let mut buf = ReadBuffer::<6>::new();
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());

        let mut stream = Cursor::new(b"hello world you are amazing!");

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"hello ", buf.view());

        assert_eq!(b"hello ", buf.consume_next(6).unwrap());
        assert_eq!(0, buf.available());
        assert_eq!(b"", buf.view());

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"world ", buf.view());
        assert_eq!(0, buf.head);
        assert_eq!(6, buf.tail);

        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
    }

    #[test]
    fn should_compact_if_any_leftover_before_next_read() {
        let mut buf = ReadBuffer::<6>::new();
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());

        let mut stream = Cursor::new(b"hello world you are amazing!");

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"hello ", buf.view());

        assert_eq!(b"he", buf.consume_next(2).unwrap());
        assert_eq!(4, buf.available());
        assert_eq!(b"llo ", buf.view());

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(10, buf.available());
        assert_eq!(b"llo world ", buf.view());
        assert_eq!(0, buf.head);
        assert_eq!(10, buf.tail);

        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
    }

    #[test]
    fn should_return_none_if_too_many_bytes_requested_to_view() {
        let mut buf = ReadBuffer::<6>::new();
        let mut stream = Cursor::new(b"hello world!");
        buf.read_from(&mut stream).expect("unable to read from the stream");

        assert_eq!(b"hello ", buf.view());
        assert_eq!(None, buf.consume_next(7));
    }

    #[test]
    fn should_return_empty_buffer_if_no_data() {
        let buf = ReadBuffer::<6>::new();
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
        assert_eq!(b"", buf.view());
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
    }

    #[test]
    fn should_grow_when_appending() {
        let mut buf = ReadBuffer::<1, 8>::new();
        assert_eq!(8, buf.inner.len());
        let mut stream = Cursor::new(b"hello world!");
        while stream.position() < 12 {
            buf.read_from(&mut stream).expect("unable to read from the stream");
        }
        assert_eq!(b"hello world!", buf.view());
        assert_eq!(16, buf.inner.len());
    }

    #[test]
    fn should_handle_reader_with_no_data() {
        struct StreamWithNoData;

        impl Read for StreamWithNoData {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(WouldBlock, "would block"))
            }
        }

        let mut stream = StreamWithNoData {};
        let mut buf = ReadBuffer::<8>::new();

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"", buf.view());
        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
    }

    #[test]
    fn should_propagate_errors() {
        struct FaultyStream;

        impl Read for FaultyStream {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(UnexpectedEof, "eof"))
            }
        }

        let mut stream = FaultyStream {};
        let mut buf = ReadBuffer::<8>::new();

        buf.read_from(&mut stream).expect_err("expected eof error");
    }

    #[test]
    fn should_consume_next() {
        let mut buf = ReadBuffer::<64>::new();
        let mut stream = Cursor::new(b"hello world!");
        buf.read_from(&mut stream).expect("unable to read from the stream");

        assert_eq!(b"hello world!", buf.view());
        assert_eq!(b"hello", buf.consume_next(5).unwrap());
        assert_eq!(b" ", buf.consume_next(1).unwrap());
        assert_eq!(b"world!", buf.consume_next(6).unwrap());
        assert_eq!(0, buf.available())
    }

    #[test]
    fn should_consume_next_byte() {
        let mut buf = ReadBuffer::<64>::new();
        let mut stream = Cursor::new(b"hello world!");
        buf.read_from(&mut stream).expect("unable to read from the stream");

        assert_eq!(b"hello world!", buf.view());
        assert_eq!(b'h', buf.consume_next_byte().unwrap());
        assert_eq!(b'e', buf.consume_next_byte().unwrap());
        assert_eq!(b'l', buf.consume_next_byte().unwrap());
        assert_eq!(b'l', buf.consume_next_byte().unwrap());
        assert_eq!(b'o', buf.consume_next_byte().unwrap());
        assert_eq!(b' ', buf.consume_next_byte().unwrap());
        assert_eq!(b"world!", buf.consume_next(6).unwrap());
        assert_eq!(0, buf.available())
    }

    #[test]
    fn should_view_last() {
        let mut buf = ReadBuffer::<64>::new();
        let mut stream = Cursor::new(b"hello world!");
        buf.read_from(&mut stream).expect("unable to read from the stream");

        assert_eq!(b"hello world!", buf.view());
        assert_eq!(b"world!", buf.view_last(6));
        assert_eq!(12, buf.available())
    }
}
