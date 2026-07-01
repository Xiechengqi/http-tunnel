use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use http_tunnel_protocol::{
    types::{
        decode_payload, decode_ws_message, encode_payload, encode_ws_message, ErrorPayload,
        WsClose, WsMessageKind, WsOpen,
    },
    Frame, FrameType,
};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub async fn forward_one_websocket(
    tx: mpsc::Sender<Frame>,
    target: String,
    stream_id: u64,
    open: WsOpen,
    mut browser_rx: mpsc::Receiver<Message>,
) {
    if let Err(error) =
        forward_one_websocket_inner(&tx, target, stream_id, open, &mut browser_rx).await
    {
        let payload = encode_payload(&ErrorPayload {
            code: "local_target_failed".to_string(),
            message: error.to_string(),
        })
        .unwrap_or_default();
        let _ = tx
            .send(Frame::new(FrameType::Error, stream_id, payload))
            .await;
    }
}

async fn forward_one_websocket_inner(
    tx: &mpsc::Sender<Frame>,
    target: String,
    stream_id: u64,
    open: WsOpen,
    browser_rx: &mut mpsc::Receiver<Message>,
) -> anyhow::Result<()> {
    let url = websocket_target_url(&target, &open.path)?;
    let (local_ws, _) = connect_async(&url)
        .await
        .with_context(|| format!("connect local websocket {url}"))?;
    tx.send(Frame::new(FrameType::WsAccepted, stream_id, Vec::new()))
        .await
        .context("send websocket accepted")?;

    let (mut local_tx, mut local_rx) = local_ws.split();
    loop {
        tokio::select! {
            from_browser = browser_rx.recv() => {
                let Some(message) = from_browser else {
                    break;
                };
                let is_close = matches!(message, Message::Close(_));
                local_tx.send(message).await.context("send websocket message to local target")?;
                if is_close {
                    break;
                }
            }
            from_local = local_rx.next() => {
                let Some(message) = from_local else {
                    break;
                };
                let message = message.context("read local websocket message")?;
                let is_close = matches!(message, Message::Close(_));
                tx.send(ws_message_to_frame(stream_id, message))
                    .await
                    .context("send websocket message to tunnel")?;
                if is_close {
                    break;
                }
            }
        }
    }

    let _ = tx
        .send(Frame::new(
            FrameType::WsClose,
            stream_id,
            encode_payload(&WsClose {
                code: None,
                reason: None,
            })
            .unwrap_or_default(),
        ))
        .await;
    Ok(())
}

pub fn frame_to_ws_message(payload: &[u8]) -> anyhow::Result<Message> {
    let (kind, payload) = decode_ws_message(payload)?;
    Ok(match kind {
        WsMessageKind::Text => Message::Text(String::from_utf8_lossy(payload).to_string()),
        WsMessageKind::Binary => Message::Binary(payload.to_vec()),
        WsMessageKind::Ping => Message::Ping(payload.to_vec()),
        WsMessageKind::Pong => Message::Pong(payload.to_vec()),
        WsMessageKind::Close => decode_ws_close_message(payload),
    })
}

fn ws_message_to_frame(stream_id: u64, message: Message) -> Frame {
    match message {
        Message::Text(text) => Frame::new(
            FrameType::WsMessage,
            stream_id,
            encode_ws_message(WsMessageKind::Text, text.as_bytes()),
        ),
        Message::Binary(bytes) => Frame::new(
            FrameType::WsMessage,
            stream_id,
            encode_ws_message(WsMessageKind::Binary, &bytes),
        ),
        Message::Ping(bytes) => Frame::new(
            FrameType::WsMessage,
            stream_id,
            encode_ws_message(WsMessageKind::Ping, &bytes),
        ),
        Message::Pong(bytes) => Frame::new(
            FrameType::WsMessage,
            stream_id,
            encode_ws_message(WsMessageKind::Pong, &bytes),
        ),
        Message::Close(close) => {
            let payload = close
                .map(|frame| WsClose {
                    code: Some(frame.code.into()),
                    reason: Some(frame.reason.to_string()),
                })
                .and_then(|close| encode_payload(&close).ok())
                .unwrap_or_default();
            Frame::new(
                FrameType::WsMessage,
                stream_id,
                encode_ws_message(WsMessageKind::Close, &payload),
            )
        }
        Message::Frame(_) => Frame::new(
            FrameType::WsMessage,
            stream_id,
            encode_ws_message(WsMessageKind::Close, &[]),
        ),
    }
}

pub fn frame_to_ws_close(payload: &[u8]) -> Message {
    decode_payload::<WsClose>(payload)
        .map(ws_close_to_message)
        .unwrap_or_else(|_| Message::Close(None))
}

fn decode_ws_close_message(payload: &[u8]) -> Message {
    decode_payload::<WsClose>(payload)
        .map(ws_close_to_message)
        .unwrap_or_else(|_| Message::Close(None))
}

fn ws_close_to_message(close: WsClose) -> Message {
    let Some(code) = close.code else {
        return Message::Close(None);
    };
    Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(code),
        reason: close.reason.unwrap_or_default().into(),
    }))
}

fn websocket_target_url(target: &str, path: &str) -> anyhow::Result<String> {
    let target = target.trim_end_matches('/');
    let scheme = if target.starts_with("https://") {
        "wss"
    } else if target.starts_with("http://") {
        "ws"
    } else if target.starts_with("ws://") || target.starts_with("wss://") {
        return Ok(format!("{target}{path}"));
    } else {
        anyhow::bail!("target must start with http://, https://, ws://, or wss://");
    };
    let rest = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
        .expect("validated target prefix");
    Ok(format!("{scheme}://{rest}{path}"))
}
