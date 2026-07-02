use super::ProxyRequestMeta;
use crate::state::{ActiveSession, AppState, PendingStream, PendingStreamType};
use axum::{
    body::Body,
    http::{header, HeaderMap, Request, StatusCode},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use http_tunnel_common::headers::filtered_headers;
use http_tunnel_protocol::{
    types::{
        decode_payload, decode_ws_message, encode_payload, encode_ws_message, ErrorPayload,
        WsClose, WsMessageKind, WsOpen,
    },
    Frame, FrameType,
};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::{
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{CloseFrame, Role},
        Message as TungsteniteMessage,
    },
    WebSocketStream,
};

pub async fn forward_ws_to_tunnel(
    state: AppState,
    session: ActiveSession,
    subdomain: String,
    mut req: Request<Body>,
    meta: ProxyRequestMeta,
) -> Response {
    let stream_id = state.next_stream_id();
    let request_log_id = http_tunnel_common::ids::generate_request_id();
    let started_at = Instant::now();
    let cfg = state.config.read().await.clone();
    if super::header_bytes(req.headers()) > cfg.max_header_bytes.min(usize::MAX as u64) as usize {
        return super::tunnel_error(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "headers_too_large",
            "request headers are too large",
        );
    }
    if state.pending_streams.read().await.len() >= cfg.max_concurrent_streams {
        return super::tunnel_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_streams",
            "too many concurrent tunnel streams",
        );
    }
    let Some(sec_key) = websocket_key(req.headers()) else {
        return super::tunnel_error(
            StatusCode::BAD_REQUEST,
            "bad_websocket_request",
            "invalid websocket upgrade request",
        );
    };
    let on_upgrade = hyper::upgrade::on(&mut req);
    let (parts, _) = req.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let mut headers = filtered_headers(&parts.headers)
        .iter()
        .filter(|(name, _)| super::forwarded_header_allowed(name))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect::<Vec<_>>();
    headers.push(("x-http-tunnel-subdomain".to_string(), subdomain));
    if let Some(host) = meta.host.as_deref() {
        headers.push(("x-forwarded-host".to_string(), host.to_string()));
    }
    headers.push(("x-forwarded-for".to_string(), meta.forwarded_for.clone()));
    headers.push(("x-forwarded-proto".to_string(), cfg.public_scheme.clone()));

    let _ = sqlx::query(
        "INSERT INTO request_logs (id, tunnel_id, session_id, request_type, method, path, host, remote_ip, user_agent, bytes_in, bytes_out, ws_message_count) \
         VALUES (?1, ?2, ?3, 'ws', 'WEBSOCKET', ?4, ?5, ?6, ?7, 0, 0, 0)",
    )
    .bind(&request_log_id)
    .bind(&session.tunnel_id)
    .bind(&session.session_id)
    .bind(&path)
    .bind(meta.host.as_deref())
    .bind(&meta.remote_ip)
    .bind(meta.user_agent.as_deref())
    .execute(&state.pool)
    .await;

    let (tx, rx) = mpsc::channel::<Frame>(256);
    state
        .insert_pending_stream(
            stream_id,
            PendingStream {
                tunnel_id: session.tunnel_id.clone(),
                session_id: session.session_id.clone(),
                stream_type: PendingStreamType::WebSocket,
                tx,
                session_metrics: session.metrics.clone(),
            },
        )
        .await;
    let open = WsOpen { path, headers };
    if super::send_frame(
        &session,
        Frame::new(
            FrameType::WsOpen,
            stream_id,
            encode_payload(&open).unwrap_or_default(),
        ),
    )
    .await
    .is_err()
    {
        state.remove_pending_stream(stream_id).await;
        return super::tunnel_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "tunnel_offline",
            "tunnel is offline",
        );
    }

    let mut rx = rx;
    let accept_timeout = Duration::from_secs(cfg.request_timeout_seconds.max(1));
    let accepted = tokio::time::timeout(accept_timeout, async {
        while let Some(frame) = rx.recv().await {
            match frame.frame_type {
                FrameType::WsAccepted => return Ok(()),
                FrameType::Error => {
                    let payload = decode_payload::<ErrorPayload>(&frame.payload)
                        .map_err(|error| error.to_string())?;
                    return Err(format!("{}: {}", payload.code, payload.message));
                }
                FrameType::Cancel | FrameType::WsClose => {
                    return Err("websocket rejected".to_string());
                }
                _ => {}
            }
        }
        Err("tunnel disconnected before websocket accept".to_string())
    })
    .await;

    match accepted {
        Ok(Ok(())) => {
            update_ws_log(
                &state,
                &request_log_id,
                StatusCode::SWITCHING_PROTOCOLS.as_u16(),
                None,
                None,
                None,
                None,
                None,
                started_at,
            )
            .await;
            let response = websocket_switching_protocols_response(&sec_key);
            tokio::spawn(handle_public_ws(
                state,
                session,
                stream_id,
                request_log_id,
                started_at,
                on_upgrade,
                rx,
            ));
            response
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "websocket local target failed before upgrade");
            state.remove_pending_stream(stream_id).await;
            update_ws_log(
                &state,
                &request_log_id,
                StatusCode::BAD_GATEWAY.as_u16(),
                Some("local_target_failed"),
                None,
                None,
                None,
                None,
                started_at,
            )
            .await;
            super::tunnel_error(
                StatusCode::BAD_GATEWAY,
                "local_target_failed",
                "client local target failed",
            )
        }
        Err(_) => {
            state.remove_pending_stream(stream_id).await;
            let _ = super::send_frame(
                &session,
                Frame::new(
                    FrameType::Cancel,
                    stream_id,
                    b"websocket_accept_timeout".to_vec(),
                ),
            )
            .await;
            update_ws_log(
                &state,
                &request_log_id,
                StatusCode::GATEWAY_TIMEOUT.as_u16(),
                Some("tunnel_timeout"),
                None,
                None,
                None,
                None,
                started_at,
            )
            .await;
            super::tunnel_error(
                StatusCode::GATEWAY_TIMEOUT,
                "tunnel_timeout",
                "websocket accept timed out",
            )
        }
    }
}

async fn handle_public_ws(
    state: AppState,
    session: ActiveSession,
    stream_id: u64,
    request_log_id: String,
    started_at: Instant,
    on_upgrade: OnUpgrade,
    tunnel_rx: mpsc::Receiver<Frame>,
) {
    let cfg = state.config.read().await.clone();
    let max_ws_message_bytes = cfg.max_ws_message_bytes;
    let idle_timeout = Duration::from_secs(cfg.idle_timeout_seconds.max(1));
    let upgraded = match on_upgrade.await {
        Ok(upgraded) => upgraded,
        Err(error) => {
            tracing::warn!(%error, "websocket upgrade failed after accept");
            let _ = super::send_frame(
                &session,
                Frame::new(FrameType::Cancel, stream_id, b"upgrade_failed".to_vec()),
            )
            .await;
            state.remove_pending_stream(stream_id).await;
            update_ws_log(
                &state,
                &request_log_id,
                StatusCode::BAD_GATEWAY.as_u16(),
                Some("websocket_upgrade_failed"),
                None,
                None,
                None,
                None,
                started_at,
            )
            .await;
            return;
        }
    };
    let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
    relay_public_ws(
        PublicWsRelay {
            state,
            session,
            stream_id,
            request_log_id,
            started_at,
            max_ws_message_bytes,
            idle_timeout,
        },
        socket,
        tunnel_rx,
    )
    .await;
}

struct PublicWsRelay {
    state: AppState,
    session: ActiveSession,
    stream_id: u64,
    request_log_id: String,
    started_at: Instant,
    max_ws_message_bytes: usize,
    idle_timeout: Duration,
}

async fn relay_public_ws<S>(
    relay: PublicWsRelay,
    socket: WebSocketStream<S>,
    mut tunnel_rx: mpsc::Receiver<Frame>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let PublicWsRelay {
        state,
        session,
        stream_id,
        request_log_id,
        started_at,
        max_ws_message_bytes,
        idle_timeout,
    } = relay;
    let (mut browser_tx, mut browser_rx) = socket.split();
    let (activity_tx, mut activity_rx) = mpsc::channel::<()>(8);
    let bytes_in = Arc::new(AtomicI64::new(0));
    let bytes_out = Arc::new(AtomicI64::new(0));
    let message_count = Arc::new(AtomicI64::new(0));
    let close_code = Arc::new(AtomicI64::new(0));
    let close_reason = Arc::new(tokio::sync::RwLock::new(None::<String>));

    let session_to_client = session.clone();
    let browser_bytes_in = bytes_in.clone();
    let browser_message_count = message_count.clone();
    let browser_close_code = close_code.clone();
    let browser_close_reason = close_reason.clone();
    let browser_to_tunnel = tokio::spawn(async move {
        while let Some(message) = browser_rx.next().await {
            let Ok(message) = message else {
                break;
            };
            let _ = activity_tx.try_send(());
            let frame = match message {
                TungsteniteMessage::Text(text) => {
                    browser_bytes_in.fetch_add(text.len() as i64, Ordering::Relaxed);
                    session_to_client.metrics.add_bytes_in(text.len());
                    session_to_client.tunnel_traffic.add_bytes_in(text.len());
                    browser_message_count.fetch_add(1, Ordering::Relaxed);
                    Frame::new(
                        FrameType::WsMessage,
                        stream_id,
                        encode_ws_message(WsMessageKind::Text, text.as_bytes()),
                    )
                }
                TungsteniteMessage::Binary(bytes) => {
                    browser_bytes_in.fetch_add(bytes.len() as i64, Ordering::Relaxed);
                    session_to_client.metrics.add_bytes_in(bytes.len());
                    session_to_client.tunnel_traffic.add_bytes_in(bytes.len());
                    browser_message_count.fetch_add(1, Ordering::Relaxed);
                    Frame::new(
                        FrameType::WsMessage,
                        stream_id,
                        encode_ws_message(WsMessageKind::Binary, &bytes),
                    )
                }
                TungsteniteMessage::Ping(bytes) => Frame::new(
                    FrameType::WsMessage,
                    stream_id,
                    encode_ws_message(WsMessageKind::Ping, &bytes),
                ),
                TungsteniteMessage::Pong(bytes) => Frame::new(
                    FrameType::WsMessage,
                    stream_id,
                    encode_ws_message(WsMessageKind::Pong, &bytes),
                ),
                TungsteniteMessage::Close(close) => {
                    let payload = tungstenite_close_to_payload(close);
                    if let Some(code) = payload.code {
                        browser_close_code.store(i64::from(code), Ordering::Relaxed);
                    }
                    if let Some(reason) = payload.reason.as_ref().filter(|value| !value.is_empty())
                    {
                        *browser_close_reason.write().await = Some(reason.clone());
                    }
                    Frame::new(
                        FrameType::WsClose,
                        stream_id,
                        encode_payload(&payload).unwrap_or_default(),
                    )
                }
                TungsteniteMessage::Frame(_) => continue,
            };
            if frame.payload.len().saturating_sub(1) > max_ws_message_bytes {
                let _ = super::send_frame(
                    &session_to_client,
                    Frame::new(
                        FrameType::Cancel,
                        stream_id,
                        b"ws_message_too_large".to_vec(),
                    ),
                )
                .await;
                break;
            }
            if super::send_frame(&session_to_client, frame).await.is_err() {
                break;
            }
        }
        let _ = super::send_frame(
            &session_to_client,
            Frame::new(
                FrameType::WsClose,
                stream_id,
                encode_payload(&WsClose {
                    code: None,
                    reason: None,
                })
                .unwrap_or_default(),
            ),
        )
        .await;
    });

    loop {
        let frame = tokio::select! {
            frame = tunnel_rx.recv() => frame,
            activity = activity_rx.recv() => {
                if activity.is_some() {
                    continue;
                }
                tunnel_rx.recv().await
            },
            _ = tokio::time::sleep(idle_timeout) => {
                let _ = browser_tx
                    .send(TungsteniteMessage::Close(Some(CloseFrame {
                        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Away,
                        reason: "idle timeout".into(),
                    })))
                    .await;
                break;
            }
        };
        let Some(frame) = frame else {
            break;
        };
        match frame.frame_type {
            FrameType::WsMessage => {
                let Ok((kind, payload)) = decode_ws_message(&frame.payload) else {
                    break;
                };
                if payload.len() > max_ws_message_bytes {
                    let _ = browser_tx
                        .send(TungsteniteMessage::Close(Some(CloseFrame {
                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Size,
                            reason: "message too large".into(),
                        })))
                        .await;
                    break;
                }
                let send_result = match kind {
                    WsMessageKind::Text => {
                        let text = String::from_utf8_lossy(payload).to_string();
                        bytes_out.fetch_add(payload.len() as i64, Ordering::Relaxed);
                        session.metrics.add_bytes_out(payload.len());
                        session.tunnel_traffic.add_bytes_out(payload.len());
                        message_count.fetch_add(1, Ordering::Relaxed);
                        browser_tx.send(TungsteniteMessage::Text(text)).await
                    }
                    WsMessageKind::Binary => {
                        bytes_out.fetch_add(payload.len() as i64, Ordering::Relaxed);
                        session.metrics.add_bytes_out(payload.len());
                        session.tunnel_traffic.add_bytes_out(payload.len());
                        message_count.fetch_add(1, Ordering::Relaxed);
                        browser_tx
                            .send(TungsteniteMessage::Binary(payload.to_vec()))
                            .await
                    }
                    WsMessageKind::Ping => {
                        browser_tx
                            .send(TungsteniteMessage::Ping(payload.to_vec()))
                            .await
                    }
                    WsMessageKind::Pong => {
                        browser_tx
                            .send(TungsteniteMessage::Pong(payload.to_vec()))
                            .await
                    }
                    WsMessageKind::Close => {
                        let close = decode_payload::<WsClose>(payload).ok();
                        if let Some(close) = close.as_ref() {
                            if let Some(code) = close.code {
                                close_code.store(i64::from(code), Ordering::Relaxed);
                            }
                            if let Some(reason) =
                                close.reason.as_ref().filter(|value| !value.is_empty())
                            {
                                *close_reason.write().await = Some(reason.clone());
                            }
                        }
                        browser_tx.send(payload_to_tungstenite_close(payload)).await
                    }
                };
                if send_result.is_err() || kind == WsMessageKind::Close {
                    break;
                }
            }
            FrameType::WsClose | FrameType::Cancel | FrameType::Error => break,
            _ => {}
        }
    }

    browser_to_tunnel.abort();
    state.remove_pending_stream(stream_id).await;
    let close_code_value = match close_code.load(Ordering::Relaxed) {
        0 => None,
        code => Some(code),
    };
    let close_reason_value = close_reason.read().await.clone();
    update_ws_log(
        &state,
        &request_log_id,
        StatusCode::SWITCHING_PROTOCOLS.as_u16(),
        None,
        Some(bytes_in.load(Ordering::Relaxed)),
        Some(bytes_out.load(Ordering::Relaxed)),
        Some(message_count.load(Ordering::Relaxed)),
        close_code_value,
        started_at,
    )
    .await;
    if close_reason_value.is_some() {
        let _ = sqlx::query("UPDATE request_logs SET ws_close_reason = ?1 WHERE id = ?2")
            .bind(close_reason_value.as_deref())
            .bind(&request_log_id)
            .execute(&state.pool)
            .await;
    }
    let _ = super::send_frame(
        &session,
        Frame::new(
            FrameType::WsClose,
            stream_id,
            encode_payload(&WsClose {
                code: None,
                reason: None,
            })
            .unwrap_or_default(),
        ),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn update_ws_log(
    state: &AppState,
    request_log_id: &str,
    status: u16,
    error: Option<&str>,
    bytes_in: Option<i64>,
    bytes_out: Option<i64>,
    message_count: Option<i64>,
    close_code: Option<i64>,
    started_at: Instant,
) {
    let _ = sqlx::query(
        "UPDATE request_logs SET status = ?1, completed_at = CURRENT_TIMESTAMP, duration_ms = ?2, error = ?3, \
         bytes_in = COALESCE(?4, bytes_in), bytes_out = COALESCE(?5, bytes_out), \
         ws_message_count = COALESCE(?6, ws_message_count), ws_close_code = COALESCE(?7, ws_close_code) \
         WHERE id = ?8",
    )
    .bind(i64::from(status))
    .bind(i64::try_from(started_at.elapsed().as_millis()).unwrap_or(i64::MAX))
    .bind(error)
    .bind(bytes_in)
    .bind(bytes_out)
    .bind(message_count)
    .bind(close_code)
    .bind(request_log_id)
    .execute(&state.pool)
    .await;
}

fn websocket_key(headers: &HeaderMap) -> Option<String> {
    if headers
        .get("sec-websocket-version")
        .and_then(|value| value.to_str().ok())
        != Some("13")
    {
        return None;
    }
    headers
        .get("sec-websocket-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn websocket_switching_protocols_response(sec_key: &str) -> Response {
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(
            "sec-websocket-accept",
            derive_accept_key(sec_key.as_bytes()),
        )
        .body(Body::empty())
        .unwrap_or_else(|_| {
            super::tunnel_error(
                StatusCode::BAD_GATEWAY,
                "websocket_upgrade_failed",
                "failed to build websocket upgrade response",
            )
        })
}

fn tungstenite_close_to_payload(close: Option<CloseFrame>) -> WsClose {
    close
        .map(|frame| WsClose {
            code: Some(frame.code.into()),
            reason: Some(frame.reason.to_string()),
        })
        .unwrap_or(WsClose {
            code: None,
            reason: None,
        })
}

fn payload_to_tungstenite_close(payload: &[u8]) -> TungsteniteMessage {
    let Ok(close) = decode_payload::<WsClose>(payload) else {
        return TungsteniteMessage::Close(None);
    };
    TungsteniteMessage::Close(close.code.map(|code| CloseFrame {
        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(code),
        reason: close.reason.unwrap_or_default().into(),
    }))
}
