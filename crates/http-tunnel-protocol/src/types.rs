use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub target: String,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<u8>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub reconnect_token: Option<String>,
    #[serde(default)]
    pub client_source: Option<ClientSourceReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloAck {
    pub accepted: bool,
    pub message: Option<String>,
    #[serde(default)]
    pub reconnect_token: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientSourceReport {
    pub public_ip: String,
    #[serde(default)]
    pub checked_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestStart {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseStart {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WsOpen {
    pub path: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WsClose {
    pub code: Option<u16>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WsMessageKind {
    Text = 1,
    Binary = 2,
    Ping = 9,
    Pong = 10,
    Close = 8,
}

impl TryFrom<u8> for WsMessageKind {
    type Error = crate::ProtocolError;

    fn try_from(value: u8) -> crate::Result<Self> {
        match value {
            1 => Ok(Self::Text),
            2 => Ok(Self::Binary),
            8 => Ok(Self::Close),
            9 => Ok(Self::Ping),
            10 => Ok(Self::Pong),
            other => Err(crate::ProtocolError::Json(format!(
                "unknown websocket message kind: {other}"
            ))),
        }
    }
}

pub fn encode_ws_message(kind: WsMessageKind, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 1);
    out.push(kind as u8);
    out.extend_from_slice(payload);
    out
}

pub fn decode_ws_message(payload: &[u8]) -> crate::Result<(WsMessageKind, &[u8])> {
    let Some((kind, rest)) = payload.split_first() else {
        return Err(crate::ProtocolError::Json(
            "empty websocket message payload".to_string(),
        ));
    };
    Ok((WsMessageKind::try_from(*kind)?, rest))
}

pub fn encode_payload<T: Serialize>(value: &T) -> crate::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|error| crate::ProtocolError::Json(error.to_string()))
}

pub fn decode_payload<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> crate::Result<T> {
    serde_json::from_slice(payload).map_err(|error| crate::ProtocolError::Json(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_accepts_legacy_payload_without_version_fields() {
        let hello: Hello = serde_json::from_str(r#"{"target":"http://127.0.0.1:3000"}"#).unwrap();
        assert_eq!(hello.target, "http://127.0.0.1:3000");
        assert_eq!(hello.client_version, None);
        assert_eq!(hello.protocol_version, None);
        assert!(hello.capabilities.is_empty());
        assert_eq!(hello.reconnect_token, None);
        assert_eq!(hello.client_source, None);
    }

    #[test]
    fn hello_accepts_client_source_report() {
        let hello: Hello = serde_json::from_str(
            r#"{"target":"http://127.0.0.1:3000","client_source":{"public_ip":"8.8.8.8","checked_at_unix_seconds":123}}"#,
        )
        .unwrap();
        assert_eq!(
            hello.client_source,
            Some(ClientSourceReport {
                public_ip: "8.8.8.8".to_string(),
                checked_at_unix_seconds: Some(123),
            })
        );
    }
}
