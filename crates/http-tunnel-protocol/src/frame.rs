use crate::{ProtocolError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FrameType {
    Hello = 1,
    HelloAck = 2,
    Ping = 3,
    Pong = 4,
    RequestStart = 10,
    RequestBody = 11,
    RequestEnd = 12,
    ResponseStart = 13,
    ResponseBody = 14,
    ResponseEnd = 15,
    WsOpen = 20,
    WsAccepted = 21,
    WsMessage = 22,
    WsClose = 23,
    Cancel = 30,
    Error = 31,
    Goaway = 32,
}

impl TryFrom<u8> for FrameType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Ping),
            4 => Ok(Self::Pong),
            10 => Ok(Self::RequestStart),
            11 => Ok(Self::RequestBody),
            12 => Ok(Self::RequestEnd),
            13 => Ok(Self::ResponseStart),
            14 => Ok(Self::ResponseBody),
            15 => Ok(Self::ResponseEnd),
            20 => Ok(Self::WsOpen),
            21 => Ok(Self::WsAccepted),
            22 => Ok(Self::WsMessage),
            23 => Ok(Self::WsClose),
            30 => Ok(Self::Cancel),
            31 => Ok(Self::Error),
            32 => Ok(Self::Goaway),
            other => Err(ProtocolError::UnknownFrameType(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub frame_type: FrameType,
    pub flags: u16,
    pub stream_id: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(frame_type: FrameType, stream_id: u64, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            frame_type,
            flags: 0,
            stream_id,
            payload: payload.into(),
        }
    }
}
