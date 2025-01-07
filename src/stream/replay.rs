use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use std::fs::File;
use std::io;
use std::io::{BufReader, Read, Write};
use std::path::Path;

pub struct ReplayStream<S> {
    inner: S,
}

impl ReplayStream<BufReader<File>> {
    pub fn from_file(path: impl AsRef<Path>) -> io::Result<ReplayStream<BufReader<File>>> {
        Ok(Self {
            inner: BufReader::new(File::open(path)?),
        })
    }
}

impl<S: Read> Read for ReplayStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<S> Write for ReplayStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<S> ConnectionInfoProvider for ReplayStream<S> {
    fn connection_info(&self) -> &ConnectionInfo {
        Box::leak(Box::new(ConnectionInfo::default()))
    }
}
