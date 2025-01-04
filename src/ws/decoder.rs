use std::io;
use std::io::{Read, Write};

use crate::util::into_array;
use crate::ws::{protocol, ReadBuffer, WebsocketFrame};

#[derive(Debug)]
pub struct Decoder {
    buffer: ReadBuffer,
    decode_state: DecodeState,
    fin: bool,
    payload_length: usize,
    op_code: u8,
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
        }
    }

    #[inline]
    pub fn decode_next<S: Read + Write>(&mut self, stream: &mut S) -> io::Result<Option<WebsocketFrame>> {
        loop {
            let available = self.buffer.available();
            match self.decode_state {
                DecodeState::ReadingHeader => {
                    if available > 0 {
                        let b = self.buffer.consume_next(1)[0];
                        let fin = ((b & protocol::FIN_MASK) >> 7) == 1;
                        let rsv1 = (b & protocol::RSV1_MASK) >> 6;
                        let rsv2 = (b & protocol::RSV2_MASK) >> 5;
                        let rsv3 = (b & protocol::RSV3_MASK) >> 4;
                        debug_assert_eq!(0, rsv1 + rsv2 + rsv3, "non zero RSV value received");
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
                        let b = self.buffer.consume_next(1)[0];
                        let mask = (b & protocol::MASK_MASK) >> 7;
                        debug_assert_ne!(1, mask, "masking bit set on the server frame");
                        let payload_length = b & protocol::PAYLOAD_LENGTH_MASK;
                        self.payload_length = payload_length as usize;
                        match payload_length {
                            0..=125 => self.decode_state = DecodeState::ReadingPayload,
                            126 => self.decode_state = DecodeState::ReadingExtendedPayloadLength2,
                            127 => self.decode_state = DecodeState::ReadingExtendedPayloadLength8,
                            _ => unreachable!(),
                        }
                    } else {
                        break;
                    }
                }
                DecodeState::ReadingExtendedPayloadLength2 => {
                    if available >= 2 {
                        let bytes = self.buffer.consume_next(2);
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
                        let bytes = self.buffer.consume_next(8);
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
                        let payload = self.buffer.consume_next(payload_length);
                        let frame = match self.op_code {
                            protocol::op::TEXT_FRAME => WebsocketFrame::Text(self.fin, payload),
                            protocol::op::BINARY_FRAME => WebsocketFrame::Binary(self.fin, payload),
                            protocol::op::CONTINUATION_FRAME => WebsocketFrame::Continuation(self.fin, payload),
                            protocol::op::PING => WebsocketFrame::Ping(payload),
                            protocol::op::CONNECTION_CLOSE => WebsocketFrame::Close(payload),
                            _ => panic!("unknown op code: {}", self.op_code),
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
        self.buffer.read_from(stream)?;
        Ok(None)
    }
}
