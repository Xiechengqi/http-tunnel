pub const MAGIC: [u8; 2] = *b"HT";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 2 + 1 + 1 + 2 + 8 + 4;
pub const MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;
