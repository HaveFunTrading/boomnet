//! Fixed length buffer for reading data from the network.
//!
//! The buffer should be used when implementing protocols on top of streams. It offers
//! a number of methods to retrieve the bytes with zero-copy semantics.

use std::io::Read;
use std::{io, ptr};

use crate::util::NoBlock;

const DEFAULT_INITIAL_CAPACITY: usize = 32768;

#[derive(Debug)]
pub struct ReadBuffer<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize = DEFAULT_INITIAL_CAPACITY> {
    inner: Vec<u8>,
    ptr: *const u8,
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
        let inner = vec![0u8; INITIAL_CAPACITY];
        let ptr = inner.as_ptr();
        Self {
            inner,
            ptr,
            head: 0,
            tail: 0,
        }
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
    pub const fn consume_next(&mut self, len: usize) -> Option<&'static [u8]> {
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
    pub const unsafe fn consume_next_unchecked(&mut self, len: usize) -> &'static [u8] {
        let consumed_view = &*ptr::slice_from_raw_parts(self.ptr.add(self.head), len);
        self.head += len;
        consumed_view
    }

    #[inline]
    pub const fn consume_next_byte(&mut self) -> Option<u8> {
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
    pub const unsafe fn consume_next_byte_unchecked(&mut self) -> u8 {
        let byte = *self.ptr.add(self.head);
        self.head += 1;
        byte
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::ErrorKind::{UnexpectedEof, WouldBlock};

    use super::*;

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
