use std::io;
use std::io::Read;

use crate::util::into_array;
use crate::ws::{protocol, Error, ReadBuffer, WebsocketFrame};

#[derive(Debug)]
pub struct Decoder {
    buffer: ReadBuffer,
    decode_state: DecodeState,
    fin: bool,
    payload_length: usize,
    op_code: u8,
    needs_more_data: bool,
}

#[derive(Debug)]
enum DecodeState {
    ReadingHeader,
    ReadingPayloadLength,
    ReadingExtendedPayloadLength2,
    ReadingExtendedPayloadLength8,
    ReadingPayload,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            buffer: ReadBuffer::new(),
            decode_state: DecodeState::ReadingHeader,
            fin: false,
            op_code: 0,
            payload_length: 0,
            needs_more_data: true,
        }
    }

    #[inline]
    pub fn read<S: Read>(&mut self, stream: &mut S) -> io::Result<()> {
        if self.needs_more_data {
            self.buffer.read_all_from(stream)?;
            self.needs_more_data = false;
        }
        Ok(())
    }

    #[inline]
    pub fn decode_next(&mut self) -> Result<Option<WebsocketFrame>, Error> {
        loop {
            let available = self.buffer.available();
            match self.decode_state {
                DecodeState::ReadingHeader => {
                    if available > 0 {
                        // SAFETY: available > 0
                        let b = unsafe { self.buffer.consume_next_byte_unchecked() };
                        let fin = ((b & protocol::FIN_MASK) >> 7) == 1;
                        let rsv1 = (b & protocol::RSV1_MASK) >> 6;
                        let rsv2 = (b & protocol::RSV2_MASK) >> 5;
                        let rsv3 = (b & protocol::RSV3_MASK) >> 4;
                        if rsv1 + rsv2 + rsv3 != 0 {
                            return Err(Error::Protocol("non zero RSV value received"));
                        }
                        self.fin = fin;
                        let op_code = b & protocol::OP_CODE_MASK;
                        self.op_code = op_code;
                        self.decode_state = DecodeState::ReadingPayloadLength
                    } else {
                        break;
                    }
                }
                DecodeState::ReadingPayloadLength => {
                    if available > 0 {
                        // SAFETY: available > 0
                        let b = unsafe { self.buffer.consume_next_byte_unchecked() };
                        let mask = (b & protocol::MASK_MASK) >> 7;
                        if mask == 1 {
                            return Err(Error::Protocol("masking bit set on the server frame"));
                        }
                        let payload_length = b & protocol::PAYLOAD_LENGTH_MASK;
                        self.payload_length = payload_length as usize;
                        match payload_length {
                            0..=125 => self.decode_state = DecodeState::ReadingPayload,
                            126 => self.decode_state = DecodeState::ReadingExtendedPayloadLength2,
                            127 => self.decode_state = DecodeState::ReadingExtendedPayloadLength8,
                            // we only use 7 bits
                            _ => unsafe { std::hint::unreachable_unchecked() },
                        }
                    } else {
                        break;
                    }
                }
                DecodeState::ReadingExtendedPayloadLength2 => {
                    if available >= 2 {
                        // SAFETY: available >= 2
                        let bytes = unsafe { self.buffer.consume_next_unchecked(2) };
                        // SAFETY: we know bytes length is 2
                        let payload_length = u16::from_be_bytes(unsafe { into_array(bytes) });
                        self.payload_length = payload_length as usize;
                        self.decode_state = DecodeState::ReadingPayload;
                    } else {
                        break;
                    }
                }
                DecodeState::ReadingExtendedPayloadLength8 => {
                    if available >= 8 {
                        // SAFETY: available >= 8
                        let bytes = unsafe { self.buffer.consume_next_unchecked(8) };
                        // SAFETY: we know bytes length is 8
                        let payload_length = u64::from_be_bytes(unsafe { into_array(bytes) });
                        self.payload_length = payload_length as usize;
                        self.decode_state = DecodeState::ReadingPayload;
                    } else {
                        break;
                    }
                }
                DecodeState::ReadingPayload => {
                    let payload_length = self.payload_length;
                    if available >= payload_length {
                        // SAFETY: available >= payload_length
                        let payload = unsafe { self.buffer.consume_next_unchecked(payload_length) };
                        let frame = match self.op_code {
                            protocol::op::TEXT_FRAME => WebsocketFrame::Text(self.fin, payload),
                            protocol::op::BINARY_FRAME => WebsocketFrame::Binary(self.fin, payload),
                            protocol::op::CONTINUATION_FRAME => WebsocketFrame::Continuation(self.fin, payload),
                            protocol::op::PING => WebsocketFrame::Ping(payload),
                            protocol::op::CONNECTION_CLOSE => WebsocketFrame::Close(payload),
                            _ => return Err(Error::Protocol("unknown op_code")),
                        };
                        self.decode_state = DecodeState::ReadingHeader;
                        return Ok(Some(frame));
                    } else {
                        break;
                    }
                }
            }
        }

        // await for more data
        self.needs_more_data = true;
        Ok(None)
    }
}
