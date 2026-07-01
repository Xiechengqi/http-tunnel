use crate::{
    frame::{Frame, FrameType},
    version::{HEADER_LEN, MAGIC, MAX_PAYLOAD_LEN, VERSION},
    ProtocolError, Result,
};
use bytes::{Buf, BufMut, BytesMut};

pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>> {
    if frame.payload.len() > MAX_PAYLOAD_LEN as usize {
        return Err(ProtocolError::PayloadTooLarge(frame.payload.len() as u32));
    }

    let mut out = BytesMut::with_capacity(HEADER_LEN + frame.payload.len());
    out.put_slice(&MAGIC);
    out.put_u8(VERSION);
    out.put_u8(frame.frame_type as u8);
    out.put_u16(frame.flags);
    out.put_u64(frame.stream_id);
    out.put_u32(frame.payload.len() as u32);
    out.put_slice(&frame.payload);
    Ok(out.to_vec())
}

pub fn decode_frame(input: &[u8]) -> Result<Frame> {
    if input.len() < HEADER_LEN {
        return Err(ProtocolError::TruncatedFrame);
    }
    if input[0..2] != MAGIC {
        return Err(ProtocolError::BadMagic);
    }

    let mut buf = input;
    buf.advance(2);
    let version = buf.get_u8();
    if version != VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }
    let frame_type = FrameType::try_from(buf.get_u8())?;
    let flags = buf.get_u16();
    let stream_id = buf.get_u64();
    let payload_len = buf.get_u32();
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::PayloadTooLarge(payload_len));
    }
    if buf.remaining() < payload_len as usize {
        return Err(ProtocolError::TruncatedFrame);
    }
    let payload = buf.copy_to_bytes(payload_len as usize).to_vec();
    Ok(Frame {
        frame_type,
        flags,
        stream_id,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::VERSION;

    #[test]
    fn frame_roundtrip_hello() {
        let frame = Frame::new(FrameType::Hello, 0, b"hello".to_vec());
        let encoded = encode_frame(&frame).unwrap();
        assert_eq!(decode_frame(&encoded).unwrap(), frame);
    }

    #[test]
    fn frame_roundtrip_request_start() {
        let frame = Frame::new(FrameType::RequestStart, 42, br#"{"method":"GET"}"#.to_vec());
        let encoded = encode_frame(&frame).unwrap();
        assert_eq!(decode_frame(&encoded).unwrap(), frame);
    }

    #[test]
    fn frame_rejects_bad_magic() {
        let mut encoded = encode_frame(&Frame::new(FrameType::Hello, 0, Vec::new())).unwrap();
        encoded[0] = b'X';
        assert!(matches!(
            decode_frame(&encoded),
            Err(ProtocolError::BadMagic)
        ));
    }

    #[test]
    fn frame_rejects_unsupported_version() {
        let mut encoded = encode_frame(&Frame::new(FrameType::Hello, 0, Vec::new())).unwrap();
        encoded[2] = VERSION + 1;
        assert!(matches!(
            decode_frame(&encoded),
            Err(ProtocolError::UnsupportedVersion(version)) if version == VERSION + 1
        ));
    }

    #[test]
    fn frame_rejects_truncated_payload() {
        let mut encoded = encode_frame(&Frame::new(FrameType::Hello, 0, b"abc".to_vec())).unwrap();
        encoded.pop();
        assert!(matches!(
            decode_frame(&encoded),
            Err(ProtocolError::TruncatedFrame)
        ));
    }
}
