use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("bad frame magic")]
    BadMagic,
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u8),
    #[error("unknown frame type: {0}")]
    UnknownFrameType(u8),
    #[error("truncated frame")]
    TruncatedFrame,
    #[error("payload too large: {0}")]
    PayloadTooLarge(u32),
    #[error("json payload error: {0}")]
    Json(String),
}
