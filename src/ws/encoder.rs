use std::io;
use std::io::Write;

use crate::ws::protocol;

#[inline]
pub fn send<S: Write>(stream: &mut S, fin: bool, op_code: u8, body: Option<&[u8]>) -> io::Result<()> {
    let mut header = 0u8;
    if fin {
        header |= protocol::FIN_MASK;
    }
    header |= op_code;
    stream.write_all(&header.to_be_bytes())?;
    let mut payload_length = 0u8;
    payload_length |= protocol::MASK_MASK;
    if let Some(body) = body {
        let len = body.len();
        if len <= 125 {
            payload_length |= len as u8;
            stream.write_all(&payload_length.to_be_bytes())?;
        } else if len <= u16::MAX as usize {
            payload_length |= 126;
            let extended_payload_length = len as u16;
            stream.write_all(&payload_length.to_be_bytes())?;
            stream.write_all(&extended_payload_length.to_be_bytes())?;
        } else if len <= u64::MAX as usize {
            payload_length |= 127;
            let extended_payload_length = len as u64;
            stream.write_all(&payload_length.to_be_bytes())?;
            stream.write_all(&extended_payload_length.to_be_bytes())?;
        }
    } else {
        stream.write_all(&payload_length.to_be_bytes())?;
    }
    let masking_key = 0u32;
    stream.write_all(&masking_key.to_be_bytes())?;
    if let Some(body) = body {
        // we can send plain text as masking key is set to zero on purpose
        // this is done for performance reason as it will make XOR no-op
        stream.write_all(body)?;
    }
    stream.flush()?;
    Ok(())
}
