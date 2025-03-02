//! Stream that uses file replay.

use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::fs::File;
use std::io;
use std::io::{BufReader, Read, Write};
use std::path::Path;

type Sequence = u64;

pub struct ReplayStream<S> {
    inner: S,
    seq: Sequence,
    last_seq: Sequence,
    bytes_read: HashMap<Sequence, usize>,
}

impl<S> Debug for ReplayStream<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayStream")
            .field("seq", &self.seq)
            .field("last_seq", &self.last_seq)
            .finish()
    }
}

impl ReplayStream<BufReader<File>> {
    pub fn from_file(recording_name: impl AsRef<str>) -> io::Result<ReplayStream<BufReader<File>>> {
        let recording_file = format!("{}.rec", recording_name.as_ref());
        let seq_file = format!("{}_seq.rec", recording_name.as_ref());

        let bytes_read = load_sequence_file(seq_file)?;
        let last_seq = *bytes_read
            .keys()
            .max()
            .ok_or_else(|| io::Error::other("sequence file is empty"))?;

        Ok(Self {
            inner: BufReader::new(File::open(recording_file)?),
            seq: 0,
            bytes_read,
            last_seq,
        })
    }
}

impl<S: Read> Read for ReplayStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let seq = self.seq;
        if seq > self.last_seq {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no more data to replay"));
        }
        self.seq += 1;
        let read = *self.bytes_read.get(&seq).unwrap_or(&0);
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, ""));
        }

        // keep reading until we have required number of bytes at that sequence
        let mut actual_read = 0;
        while actual_read != read {
            actual_read += self.inner.read(buf[actual_read..read].as_mut())?;
        }

        Ok(actual_read)
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

fn load_sequence_file(file: impl AsRef<Path>) -> io::Result<HashMap<Sequence, usize>> {
    let mut map = HashMap::new();
    let mut reader = BufReader::with_capacity(16, File::open(file)?);
    let mut bytes = [0u8; 16];
    loop {
        match reader.read(&mut bytes)? {
            0 => break,
            1..15 => return Err(io::Error::other("incomplete sequence file")),
            _ => {}
        }
        let (seq, read) = bytes.split_at(8);
        let seq = u64::from_le_bytes(seq.try_into().map_err(io::Error::other)?);
        let read = usize::from_le_bytes(read.try_into().map_err(io::Error::other)?);
        map.insert(seq, read);
    }
    Ok(map)
}
