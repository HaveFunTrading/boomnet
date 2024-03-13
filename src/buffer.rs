use std::io::Read;
use std::{io, ptr};

use crate::util::NoBlock;

const DEFAULT_INITIAL_CAPACITY: usize = 32768;

#[derive(Debug)]
pub struct ReadBuffer<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize = DEFAULT_INITIAL_CAPACITY> {
    inner: Vec<u8>,
    head: usize,
    tail: usize,
}

impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> Default for ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize> ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
    pub fn new() -> ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY> {
        assert!(
            CHUNK_SIZE <= DEFAULT_INITIAL_CAPACITY,
            "CHUNK_SIZE ({CHUNK_SIZE}) must be less or equal than {DEFAULT_INITIAL_CAPACITY}"
        );
        Self {
            inner: vec![0u8; INITIAL_CAPACITY],
            head: 0,
            tail: 0,
        }
    }

    #[inline]
    pub const fn available(&self) -> usize {
        self.tail - self.head
    }

    pub fn read_from<S: Read>(&mut self, stream: &mut S) -> io::Result<()> {
        #[cold]
        fn grow(buf: &mut Vec<u8>) {
            buf.resize(buf.len() * 2, 0u8);
        }

        #[cold]
        fn compact<const CHUNK_SIZE: usize, const INITIAL_CAPACITY: usize>(
            buf: &mut ReadBuffer<CHUNK_SIZE, INITIAL_CAPACITY>,
        ) {
            unsafe {
                ptr::copy(
                    buf.inner.as_ptr().add(buf.head),
                    buf.inner.as_mut_ptr(),
                    buf.available(),
                )
            }
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

        // ensure capacity
        if self.tail + CHUNK_SIZE > self.inner.len() {
            grow(&mut self.inner);
        }

        let read = stream
            .read(&mut self.inner[self.tail..self.tail + CHUNK_SIZE])
            .no_block()?;

        self.tail += read;
        Ok(())
    }

    #[inline]
    pub fn consume_next(&self, len: usize) -> &[u8] {
        let new_head = self.head + len;
        #[cfg(feature = "disable-checks")]
        let view = unsafe { &*ptr::slice_from_raw_parts(self.inner.as_ptr().add(self.head), len) };
        #[cfg(not(feature = "disable-checks"))]
        let view = &self.inner[self.head..self.head + len];

        // update head to the new value
        let head_ptr = (&self.head) as *const _ as *mut _;
        unsafe {
            ptr::replace(head_ptr, new_head);
        }

        view
    }

    #[inline]
    pub fn view(&self) -> &[u8] {
        &self.inner[self.head..self.tail]
    }

    #[inline]
    pub fn view_last(&self, len: usize) -> &[u8] {
        &self.inner[self.tail - len..self.tail]
    }

    #[inline]
    pub fn consume(&mut self, len: usize) {
        #[cfg(not(feature = "disable-checks"))]
        #[cold]
        fn bounds_violation(head: usize, tail: usize) -> ! {
            panic!("bounds violation:  head[{}] > tail[{}]", head, tail)
        }

        self.head += len;

        #[cfg(not(feature = "disable-checks"))]
        if self.head > self.tail {
            bounds_violation(self.head, self.tail);
        }
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

        buf.consume(6);
        assert_eq!(6, buf.available());
        assert_eq!(b"world!", buf.view());

        buf.consume(6);
        assert_eq!(0, buf.available());
        assert_eq!(b"", buf.view());

        assert_eq!(12, buf.head, "head");
        assert_eq!(12, buf.tail, "tail");
        assert_eq!(0, buf.available());

        assert_eq!(DEFAULT_INITIAL_CAPACITY, buf.inner.len());
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

        buf.consume(6);
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

        buf.consume(2);
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
    #[should_panic(expected = "bounds violation:  head[32] > tail[6]")]
    fn should_panic_if_bounds_violated_on_consume() {
        let mut buf = ReadBuffer::<6>::new();
        let mut stream = Cursor::new(b"hello world!");

        buf.read_from(&mut stream).expect("unable to read from the stream");
        assert_eq!(b"hello ", buf.view());

        buf.consume(32); // will panic
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
        assert_eq!(b"hello", buf.consume_next(5));
        assert_eq!(b" ", buf.consume_next(1));
        assert_eq!(b"world!", buf.consume_next(6));
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
