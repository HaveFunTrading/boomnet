use std::io;
use std::io::ErrorKind::{UnexpectedEof, WouldBlock};
use std::mem::MaybeUninit;
use std::ptr::copy_nonoverlapping;

pub trait NoBlock {
    type Value;

    fn no_block(self) -> io::Result<Self::Value>;
}

impl NoBlock for io::Result<usize> {
    type Value = usize;

    fn no_block(self) -> io::Result<Self::Value> {
        match self {
            Ok(0) => Err(io::Error::from(UnexpectedEof)),
            Ok(n) => Ok(n),
            Err(err) if err.kind() == WouldBlock => Ok(0),
            Err(err) => Err(err),
        }
    }
}

impl NoBlock for io::Result<()> {
    type Value = ();

    fn no_block(self) -> io::Result<Self::Value> {
        match self {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == WouldBlock => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[inline]
pub const unsafe fn into_array<const N: usize>(slice: &[u8]) -> [u8; N] {
    unsafe {
        let array = MaybeUninit::<[u8; N]>::uninit();
        copy_nonoverlapping(slice.as_ptr(), array.as_ptr() as *mut u8, slice.len());
        array.assume_init()
    }
}
