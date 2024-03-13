pub const FIN_MASK: u8 = 0b1000_0000;
pub const RSV1_MASK: u8 = 0b0100_0000;
pub const RSV2_MASK: u8 = 0b0010_0000;
pub const RSV3_MASK: u8 = 0b0001_0000;
pub const OP_CODE_MASK: u8 = 0b0000_1111;
pub const MASK_MASK: u8 = 0b1000_0000;
pub const PAYLOAD_LENGTH_MASK: u8 = 0b0111_1111;

pub mod op {
    pub const CONTINUATION_FRAME: u8 = 0x0;
    pub const TEXT_FRAME: u8 = 0x1;
    pub const BINARY_FRAME: u8 = 0x2;
    pub const CONNECTION_CLOSE: u8 = 0x8;
    pub const PING: u8 = 0x9;
    pub const PONG: u8 = 0xA;
}
