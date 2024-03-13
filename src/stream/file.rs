use std::cmp::min;
use std::fs::File;
use std::io;
use std::io::ErrorKind::UnexpectedEof;
use std::io::{BufReader, Read, Write};

pub struct FileStream<const CHUNK_SIZE: usize = 256>(BufReader<File>);

impl<const CHUNK_SIZE: usize> Read for FileStream<CHUNK_SIZE> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let up_to = min(buf.len(), CHUNK_SIZE);

        match self.0.read(&mut buf[..up_to]) {
            Ok(0) => Err(io::Error::new(UnexpectedEof, "eof")),
            Ok(n) => Ok(n),
            Err(err) => Err(err),
        }
    }
}

impl Write for FileStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TryFrom<&str> for FileStream {
    type Error = io::Error;

    fn try_from(path: &str) -> Result<Self, Self::Error> {
        let file = File::open(path)?;
        let stream = FileStream(BufReader::new(file));
        Ok(stream)
    }
}
