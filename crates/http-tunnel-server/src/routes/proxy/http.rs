use super::ProxyRequestMeta;
use crate::state::{ActiveSession, AppState, PendingStream, PendingStreamType};
use axum::{
    body::Body,
    http::{HeaderName, HeaderValue, Request, StatusCode},
    response::Response,
};
use bytes::Bytes;
use http_body_util::BodyExt;
use http_tunnel_common::{headers::filtered_headers, ids::generate_request_id};
use http_tunnel_protocol::{
    types::{decode_payload, encode_payload, ErrorPayload, RequestStart, ResponseStart},
    Frame, FrameType,
};
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub async fn forward_to_tunnel(
    state: AppState,
    session: ActiveSession,
    subdomain: String,
    req: Request<Body>,
    meta: ProxyRequestMeta,
    inspector_enabled: bool,
) -> Response {
    let cfg = state.config.read().await.clone();
    let request_timeout = Duration::from_secs(cfg.request_timeout_seconds);
    let max_body_bytes = cfg.max_body_bytes.min(usize::MAX as u64) as usize;
    let max_header_bytes = cfg.max_header_bytes.min(usize::MAX as u64) as usize;
    let stream_id = state.next_stream_id();
    let request_log_id = generate_request_id();
    let started_at = Instant::now();
    let inspector_max = cfg.inspector_max_body_preview_bytes;

    let (parts, mut body) = req.into_parts();
    if super::header_bytes(&parts.headers) > max_header_bytes {
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
    let method = parts.method.to_string();
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
    headers.push(("x-http-tunnel-subdomain".to_string(), subdomain.clone()));
    if let Some(host) = meta.host.as_deref() {
        headers.push(("x-forwarded-host".to_string(), host.to_string()));
    }
    headers.push(("x-forwarded-for".to_string(), meta.forwarded_for.clone()));
    headers.push(("x-forwarded-proto".to_string(), cfg.public_scheme.clone()));
    let mut inspection = InspectionCapture::new(
        inspector_enabled,
        request_log_id.clone(),
        inspector_max,
        &headers,
    );

    let _ = sqlx::query(
        "INSERT INTO request_logs (id, tunnel_id, session_id, request_type, method, path, host, remote_ip, user_agent, bytes_in) \
         VALUES (?1, ?2, ?3, 'http', ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(&request_log_id)
    .bind(&session.tunnel_id)
    .bind(&session.session_id)
    .bind(&method)
    .bind(&path)
    .bind(meta.host.as_deref())
    .bind(&meta.remote_ip)
    .bind(meta.user_agent.as_deref())
    .bind(0_i64)
    .execute(&state.pool)
    .await;
    inspection.insert(&state).await;

    let (response_tx, mut response_rx) = mpsc::channel::<Frame>(64);
    state
        .insert_pending_stream(
            stream_id,
            PendingStream {
                tunnel_id: session.tunnel_id.clone(),
                session_id: session.session_id.clone(),
                stream_type: PendingStreamType::Http,
                tx: response_tx,
                session_metrics: session.metrics.clone(),
            },
        )
        .await;

    let result = async {
        let start = RequestStart {
            method: method.clone(),
            path: path.clone(),
            headers,
        };
        super::send_frame(
            &session,
            Frame::new(
                FrameType::RequestStart,
                stream_id,
                encode_payload(&start).map_err(|error| error.to_string())?,
            ),
        )
        .await?;
        let mut bytes_in: usize = 0;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|error| format!("request_body_read_failed: {error}"))?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            bytes_in = bytes_in.saturating_add(data.len());
            session.metrics.add_bytes_in(data.len());
            session.tunnel_traffic.add_bytes_in(data.len());
            inspection.push_request_body(&data);
            if bytes_in > max_body_bytes {
                let _ = super::send_frame(
                    &session,
                    Frame::new(FrameType::Cancel, stream_id, b"request_too_large".to_vec()),
                )
                .await;
                inspection.update_request_body(&state).await;
                return Err("request_too_large".to_string());
            }
            if !data.is_empty() {
                super::send_frame(
                    &session,
                    Frame::new(FrameType::RequestBody, stream_id, data.to_vec()),
                )
                .await?;
            }
        }
        let _ = sqlx::query("UPDATE request_logs SET bytes_in = ?1 WHERE id = ?2")
            .bind(i64::try_from(bytes_in).unwrap_or(i64::MAX))
            .bind(&request_log_id)
            .execute(&state.pool)
            .await;
        inspection.update_request_body(&state).await;
        super::send_frame(
            &session,
            Frame::new(FrameType::RequestEnd, stream_id, Vec::new()),
        )
        .await?;

        while let Some(frame) = response_rx.recv().await {
            match frame.frame_type {
                FrameType::ResponseStart => {
                    return decode_payload::<ResponseStart>(&frame.payload)
                        .map_err(|error| error.to_string());
                }
                FrameType::ResponseBody => {}
                FrameType::Error => {
                    let payload = decode_payload::<ErrorPayload>(&frame.payload)
                        .map_err(|error| error.to_string())?;
                    return Err(format!("{}: {}", payload.code, payload.message));
                }
                FrameType::ResponseEnd => {
                    return Err("client ended response before response start".to_string());
                }
                _ => {}
            }
        }
        Err("tunnel disconnected before response start".to_string())
    };

    let result = tokio::time::timeout(request_timeout, result).await;

    match result {
        Ok(Ok(start)) => {
            let status = start.status;
            inspection
                .update_response_start(&state, status, &start.headers)
                .await;
            let (body_tx, body_rx) = mpsc::channel::<std::result::Result<Bytes, Infallible>>(64);
            let cleanup_state = state.clone();
            let cleanup_request_log_id = request_log_id.clone();
            let cleanup_session = session.clone();
            tokio::spawn(async move {
                let mut bytes_out: i64 = 0;
                let mut inspection = inspection;
                while let Some(frame) = response_rx.recv().await {
                    match frame.frame_type {
                        FrameType::ResponseBody => {
                            bytes_out = bytes_out.saturating_add(
                                i64::try_from(frame.payload.len()).unwrap_or(i64::MAX),
                            );
                            cleanup_session.metrics.add_bytes_out(frame.payload.len());
                            cleanup_session
                                .tunnel_traffic
                                .add_bytes_out(frame.payload.len());
                            inspection.push_response_body(&frame.payload);
                            if body_tx.send(Ok(Bytes::from(frame.payload))).await.is_err() {
                                let _ = super::send_frame(
                                    &cleanup_session,
                                    Frame::new(
                                        FrameType::Cancel,
                                        stream_id,
                                        b"browser_disconnected".to_vec(),
                                    ),
                                )
                                .await;
                                break;
                            }
                        }
                        FrameType::ResponseEnd | FrameType::Error | FrameType::Cancel => break,
                        _ => {}
                    }
                }
                cleanup_state.remove_pending_stream(stream_id).await;
                update_request_log(
                    &cleanup_state,
                    &cleanup_request_log_id,
                    status,
                    Some(bytes_out),
                    None,
                    started_at,
                )
                .await;
                inspection.update_response_body(&cleanup_state).await;
            });
            build_tunnel_stream_response(start, ReceiverStream::new(body_rx))
        }
        Ok(Err(error)) if error.contains("local_target_failed") => {
            state.remove_pending_stream(stream_id).await;
            update_request_log(
                &state,
                &request_log_id,
                StatusCode::BAD_GATEWAY.as_u16(),
                None,
                Some("local_target_failed"),
                started_at,
            )
            .await;
            super::tunnel_error(
                StatusCode::BAD_GATEWAY,
                "local_target_failed",
                "client local target failed",
            )
        }
        Ok(Err(error)) if error.contains("request_too_large") => {
            state.remove_pending_stream(stream_id).await;
            update_request_log(
                &state,
                &request_log_id,
                StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
                None,
                Some("request_too_large"),
                started_at,
            )
            .await;
            super::tunnel_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "request body is too large",
            )
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "tunnel request failed");
            state.remove_pending_stream(stream_id).await;
            update_request_log(
                &state,
                &request_log_id,
                StatusCode::BAD_GATEWAY.as_u16(),
                None,
                Some("local_target_failed"),
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
                Frame::new(FrameType::Cancel, stream_id, b"tunnel_timeout".to_vec()),
            )
            .await;
            update_request_log(
                &state,
                &request_log_id,
                StatusCode::GATEWAY_TIMEOUT.as_u16(),
                None,
                Some("tunnel_timeout"),
                started_at,
            )
            .await;
            super::tunnel_error(
                StatusCode::GATEWAY_TIMEOUT,
                "tunnel_timeout",
                "tunnel request timed out",
            )
        }
    }
}

fn build_tunnel_stream_response(
    start: ResponseStart,
    body: ReceiverStream<std::result::Result<Bytes, Infallible>>,
) -> Response {
    let status = StatusCode::from_u16(start.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = Response::builder().status(status);
    for (name, value) in start.headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        if http_tunnel_common::headers::is_hop_by_hop(&name) {
            continue;
        }
        let Ok(value) = HeaderValue::from_str(&value) else {
            continue;
        };
        builder = builder.header(name, value);
    }
    builder.body(Body::from_stream(body)).unwrap_or_else(|_| {
        super::tunnel_error(
            StatusCode::BAD_GATEWAY,
            "local_target_failed",
            "bad response",
        )
    })
}

#[derive(Debug, Clone)]
struct InspectionCapture {
    enabled: bool,
    request_log_id: String,
    max_body_preview_bytes: usize,
    request_headers: String,
    request_content_type: Option<String>,
    request_body_preview: Vec<u8>,
    request_body_truncated: bool,
    response_content_type: Option<String>,
    response_body_preview: Vec<u8>,
    response_body_truncated: bool,
}

impl InspectionCapture {
    fn new(
        enabled: bool,
        request_log_id: String,
        max_body_preview_bytes: usize,
        request_headers: &[(String, String)],
    ) -> Self {
        Self {
            enabled,
            request_log_id,
            max_body_preview_bytes,
            request_headers: redacted_headers_json(request_headers),
            request_content_type: header_value(request_headers, "content-type"),
            request_body_preview: Vec::new(),
            request_body_truncated: false,
            response_content_type: None,
            response_body_preview: Vec::new(),
            response_body_truncated: false,
        }
    }

    async fn insert(&self, state: &AppState) {
        if !self.enabled {
            return;
        }
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO request_inspections (request_log_id, request_headers, request_content_type) VALUES (?1, ?2, ?3)",
        )
        .bind(&self.request_log_id)
        .bind(&self.request_headers)
        .bind(self.request_content_type.as_deref())
        .execute(&state.pool)
        .await;
    }

    fn push_request_body(&mut self, chunk: &[u8]) {
        push_preview(
            chunk,
            self.max_body_preview_bytes,
            &mut self.request_body_preview,
            &mut self.request_body_truncated,
        );
    }

    fn push_response_body(&mut self, chunk: &[u8]) {
        push_preview(
            chunk,
            self.max_body_preview_bytes,
            &mut self.response_body_preview,
            &mut self.response_body_truncated,
        );
    }

    async fn update_request_body(&self, state: &AppState) {
        if !self.enabled {
            return;
        }
        let encoding = preview_encoding(
            self.request_content_type.as_deref(),
            &self.request_body_preview,
        );
        let _ = sqlx::query(
            "UPDATE request_inspections SET request_body_preview = ?1, request_body_preview_encoding = ?2, request_body_truncated = ?3, updated_at = CURRENT_TIMESTAMP \
             WHERE request_log_id = ?4",
        )
        .bind(&self.request_body_preview)
        .bind(encoding)
        .bind(self.request_body_truncated)
        .bind(&self.request_log_id)
        .execute(&state.pool)
        .await;
    }

    async fn update_response_start(
        &mut self,
        state: &AppState,
        status: u16,
        headers: &[(String, String)],
    ) {
        if !self.enabled {
            return;
        }
        self.response_content_type = header_value(headers, "content-type");
        let _ = sqlx::query(
            "UPDATE request_inspections SET response_status = ?1, response_headers = ?2, response_content_type = ?3, updated_at = CURRENT_TIMESTAMP \
             WHERE request_log_id = ?4",
        )
        .bind(i64::from(status))
        .bind(redacted_headers_json(headers))
        .bind(self.response_content_type.as_deref())
        .bind(&self.request_log_id)
        .execute(&state.pool)
        .await;
    }

    async fn update_response_body(&self, state: &AppState) {
        if !self.enabled {
            return;
        }
        let encoding = preview_encoding(
            self.response_content_type.as_deref(),
            &self.response_body_preview,
        );
        let _ = sqlx::query(
            "UPDATE request_inspections SET response_body_preview = ?1, response_body_preview_encoding = ?2, response_body_truncated = ?3, updated_at = CURRENT_TIMESTAMP \
             WHERE request_log_id = ?4",
        )
        .bind(&self.response_body_preview)
        .bind(encoding)
        .bind(self.response_body_truncated)
        .bind(&self.request_log_id)
        .execute(&state.pool)
        .await;
    }
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.to_string())
}

fn preview_encoding(content_type: Option<&str>, bytes: &[u8]) -> &'static str {
    if bytes.is_empty()
        || content_type.is_some_and(is_textual_content_type)
        || looks_like_text(bytes)
    {
        "utf8"
    } else {
        "base64"
    }
}

fn is_textual_content_type(content_type: &str) -> bool {
    let value = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    value.starts_with("text/")
        || matches!(
            value.as_str(),
            "application/json"
                | "application/javascript"
                | "application/x-www-form-urlencoded"
                | "application/xml"
                | "image/svg+xml"
        )
        || value.ends_with("+json")
        || value.ends_with("+xml")
}

fn looks_like_text(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| {
        text.chars()
            .all(|ch| ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control())
    })
}

fn push_preview(chunk: &[u8], max: usize, preview: &mut Vec<u8>, truncated: &mut bool) {
    if *truncated || chunk.is_empty() {
        return;
    }
    let remaining = max.saturating_sub(preview.len());
    if remaining == 0 {
        *truncated = true;
        return;
    }
    if chunk.len() > remaining {
        preview.extend_from_slice(&chunk[..remaining]);
        *truncated = true;
    } else {
        preview.extend_from_slice(chunk);
    }
}

fn redacted_headers_json(headers: &[(String, String)]) -> String {
    let values = headers
        .iter()
        .map(|(name, value)| {
            serde_json::json!({
                "name": name,
                "value": if sensitive_header(name) { "[redacted]" } else { value },
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string())
}

fn sensitive_header(name: &str) -> bool {
    crate::redaction::sensitive_key(name)
}

async fn update_request_log(
    state: &AppState,
    request_log_id: &str,
    status: u16,
    bytes_out: Option<i64>,
    error: Option<&str>,
    started_at: Instant,
) {
    let _ = sqlx::query(
        "UPDATE request_logs SET status = ?1, completed_at = CURRENT_TIMESTAMP, duration_ms = ?2, bytes_out = ?3, error = ?4 WHERE id = ?5",
    )
    .bind(i64::from(status))
    .bind(i64::try_from(started_at.elapsed().as_millis()).unwrap_or(i64::MAX))
    .bind(bytes_out)
    .bind(error)
    .bind(request_log_id)
    .execute(&state.pool)
    .await;
}
