//! Stream that will also record incoming and outgoing data to a file.
//!

use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
use std::fmt::{Debug, Formatter};
use std::fs::File;
use std::io;
use std::io::{BufWriter, Read, Write};

const DEFAULT_RECORDING_NAME: &str = "plain";

pub struct Recorder {
    inbound: Box<dyn Write>,
    inbound_seq: Box<dyn Write>,
    outbound: Box<dyn Write>,
}

impl Recorder {
    pub fn new(recording_name: impl AsRef<str>) -> io::Result<Self> {
        let file_in = format!("{}_inbound.rec", recording_name.as_ref());
        let file_out = format!("{}_outbound.rec", recording_name.as_ref());
        let inbound = Box::new(BufWriter::new(File::create(file_in)?));
        let outbound = Box::new(BufWriter::new(File::create(file_out)?));

        let file_seq_in = format!("{}_inbound_seq.rec", recording_name.as_ref());
        let inbound_seq = Box::new(BufWriter::new(File::create(file_seq_in)?));

        Ok(Self {
            inbound,
            inbound_seq,
            outbound,
        })
    }
    fn record_inbound(&mut self, buf: &[u8], seq: usize) -> io::Result<()> {
        self.inbound.write_all(buf)?;
        self.inbound.flush()?;
        self.inbound_seq.write_all(&seq.to_le_bytes())?;
        self.inbound_seq.write_all(&buf.len().to_le_bytes())?;
        self.inbound_seq.flush()?;
        Ok(())
    }
    fn record_outbound(&mut self, buf: &[u8]) -> io::Result<()> {
        self.outbound.write_all(buf)?;
        self.outbound.flush()
    }
}

pub struct RecordedStream<S> {
    inner: S,
    recorder: Recorder,
    inbound_seq: usize,
}

impl<S> Debug for RecordedStream<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordedStream")
            .field("seq", &self.inbound_seq)
            .finish()
    }
}

impl<S> RecordedStream<S> {
    pub fn new(stream: S, recorder: Recorder) -> RecordedStream<S> {
        Self {
            inner: stream,
            recorder,
            inbound_seq: 0,
        }
    }
}

impl<S: Read + Write> Read for RecordedStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let seq = self.inbound_seq;
        self.inbound_seq += 1;
        let read = self.inner.read(buf)?;
        self.recorder.record_inbound(&buf[..read], seq)?;
        Ok(read)
    }
}

impl<S: Read + Write> Write for RecordedStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let wrote = self.inner.write(buf)?;
        self.recorder.record_outbound(&buf[..wrote])?;
        Ok(wrote)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<S: ConnectionInfoProvider> ConnectionInfoProvider for RecordedStream<S> {
    fn connection_info(&self) -> &ConnectionInfo {
        self.inner.connection_info()
    }
}

pub trait IntoRecordedStream {
    fn into_recorded_stream(self, recording_name: impl AsRef<str>) -> RecordedStream<Self>
    where
        Self: Sized;

    fn into_default_recorded_stream(self) -> RecordedStream<Self>
    where
        Self: Sized,
    {
        self.into_recorded_stream(DEFAULT_RECORDING_NAME)
    }
}

impl<T> IntoRecordedStream for T
where
    T: Read + Write,
{
    fn into_recorded_stream(self, recording_name: impl AsRef<str>) -> RecordedStream<Self>
    where
        Self: Sized,
    {
        RecordedStream::new(self, Recorder::new(recording_name).unwrap())
    }
}
