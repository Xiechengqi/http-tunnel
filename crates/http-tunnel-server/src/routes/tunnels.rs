use crate::{
    error::{AppError, Result},
    geoip,
    net::{client_country_code_from_headers, client_ip},
    routes::admin::{
        list_response, record_admin_audit, require_admin, require_admin_write, AuditLog,
    },
    state::{
        effective_session_pool_policy, ActiveSession, AppState, RuntimeSessionMetricsSnapshot,
        SessionRuntimeMetrics,
    },
};
use axum::{
    extract::{
        connect_info::ConnectInfo,
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use http_tunnel_common::{
    api::ApiResponse,
    country::{country_code_from_name, normalize_country_code as normalize_reported_country_code},
    ids::{generate_event_id, generate_session_id, generate_tunnel_id},
    ip::parse_public_ip,
    password::hash_password,
    subdomain::validate_subdomain,
    token::{generate_token, hash_token, verify_token},
};
use http_tunnel_protocol::{
    decode_frame, encode_frame,
    types::{decode_payload, encode_payload, ClientSourceReport, Hello, HelloAck},
    version::VERSION as PROTOCOL_VERSION,
    Frame, FrameType,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{Row, SqlitePool};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

const DEFAULT_TUNNEL_LIMIT: i64 = 200;
const DEFAULT_LOG_LIMIT: i64 = 100;
const MAX_PAGE_LIMIT: i64 = 500;
const MAX_EXPORT_LIMIT: i64 = 10_000;
const CLIENT_SOURCE_UPDATE_CAPABILITY: &str = "client_source_update";

#[derive(Debug, Deserialize)]
pub struct CreateTunnelRequest {
    pub subdomain: Option<String>,
    pub ttl_seconds: Option<u64>,
    pub turnstile_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateTunnelResponse {
    pub id: String,
    pub token: String,
    pub url: String,
    pub connect_url: String,
}

#[derive(Debug, Serialize)]
pub struct TunnelRecord {
    pub id: String,
    pub subdomain: String,
    pub status: String,
    pub enabled: bool,
    pub created_at: String,
    pub expires_at: String,
    pub access_policy: String,
    pub access_token_configured: bool,
    pub access_username: Option<String>,
    pub allowed_methods: Vec<String>,
    pub blocked_path_prefixes: Vec<String>,
    pub inspector_enabled: bool,
    pub rate_limit_per_minute: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SessionRecord {
    pub id: String,
    pub connected_at: String,
    pub disconnected_at: Option<String>,
    pub disconnect_reason: Option<String>,
    pub last_seen_at: String,
    pub client_version: Option<String>,
    pub client_capabilities: Vec<String>,
    pub remote_addr: Option<String>,
    pub runtime_active: bool,
    pub runtime_active_streams: Option<usize>,
    pub runtime_bytes_in: Option<u64>,
    pub runtime_bytes_out: Option<u64>,
    pub runtime_selected_count: Option<u64>,
    pub runtime_last_selected_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct TunnelDetail {
    pub tunnel: TunnelRecord,
    pub active_session: Option<SessionRecord>,
    pub active_sessions: Vec<SessionRecord>,
    pub request_count: i64,
    pub error_count: i64,
    pub recent_requests: Vec<serde_json::Value>,
    pub recent_events: Vec<serde_json::Value>,
    pub last_error: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct RotateTokenResponse {
    pub token: String,
}

#[derive(Debug, Clone)]
struct ClientSource {
    ip: String,
    header_country_code: Option<String>,
    remote_country: Option<geoip::CountryLocation>,
}

#[derive(Debug, Clone)]
struct ResolvedClientSource {
    country_code: Option<String>,
    country: Option<String>,
    country_source: Option<&'static str>,
    reported_ip: Option<String>,
}

fn client_source(
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    cfg: &http_tunnel_common::ServerConfig,
) -> ClientSource {
    let ip = client_ip(
        headers,
        remote_addr,
        cfg.trust_proxy_headers,
        &cfg.trusted_proxy_cidrs,
    );
    let header_country_code = client_country_code_from_headers(
        headers,
        remote_addr,
        cfg.trust_proxy_headers,
        &cfg.trusted_proxy_cidrs,
    );
    let remote_country = ip
        .parse()
        .ok()
        .and_then(|ip| geoip::lookup_country(&cfg.data_dir, ip));

    ClientSource {
        ip,
        header_country_code,
        remote_country,
    }
}

fn resolve_client_source(
    cfg: &http_tunnel_common::ServerConfig,
    source: &ClientSource,
    report: Option<&ClientSourceReport>,
) -> ResolvedClientSource {
    let reported_ip = report.and_then(|report| parse_public_ip(&report.public_ip));
    let reported_country = reported_ip.and_then(|ip| geoip::lookup_country(&cfg.data_dir, ip));
    let reported_ip = reported_ip.map(|ip| ip.to_string());
    let reported_hint = report.and_then(reported_country_hint);

    if let Some(country_code) = source.header_country_code.clone() {
        return ResolvedClientSource {
            country_code: Some(country_code),
            country: None,
            country_source: Some("cf_header"),
            reported_ip,
        };
    }

    if let Some(country) = reported_country {
        return ResolvedClientSource {
            country_code: Some(country.country_code),
            country: country.country,
            country_source: Some("reported_ip"),
            reported_ip,
        };
    }

    if let Some((country_code, country)) = reported_hint {
        return ResolvedClientSource {
            country_code: Some(country_code),
            country,
            country_source: Some("client_report"),
            reported_ip,
        };
    }

    if let Some(country) = source.remote_country.clone() {
        return ResolvedClientSource {
            country_code: Some(country.country_code),
            country: country.country,
            country_source: Some("remote_geoip"),
            reported_ip,
        };
    }

    ResolvedClientSource {
        country_code: None,
        country: None,
        country_source: None,
        reported_ip,
    }
}

fn reported_country_hint(report: &ClientSourceReport) -> Option<(String, Option<String>)> {
    let country = report
        .country
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let country_code = report
        .country_code
        .as_deref()
        .and_then(normalize_reported_country_code)
        .or_else(|| {
            country
                .as_deref()
                .and_then(country_code_from_name)
                .map(ToString::to_string)
        })?;
    Some((country_code, country))
}

#[derive(Debug, Deserialize)]
pub struct TunnelPatchRequest {
    pub ttl_seconds: Option<u64>,
    pub expire_now: Option<bool>,
    pub enabled: Option<bool>,
    pub access_policy: Option<String>,
    pub access_token: Option<String>,
    pub access_username: Option<String>,
    pub access_password: Option<String>,
    pub allowed_methods: Option<Vec<String>>,
    pub blocked_path_prefixes: Option<Vec<String>>,
    pub inspector_enabled: Option<bool>,
    pub rate_limit_per_minute: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectQuery {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct OptionalTokenQuery {
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TunnelListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub status: Option<String>,
    pub subdomain: Option<String>,
    pub q: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub all: Option<bool>,
    pub status: Option<i64>,
    pub error_only: Option<bool>,
    #[serde(rename = "type")]
    pub request_type: Option<String>,
    pub q: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EventListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub kind: Option<String>,
    pub q: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<CreateTunnelRequest>,
) -> Result<Json<ApiResponse<CreateTunnelResponse>>> {
    let cfg = state.config.read().await.clone();
    if cfg.setup_required() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "setup_required",
            "setup is required",
        ));
    }
    verify_turnstile_if_required(&cfg, req.turnstile_token.as_deref()).await?;
    let create_ip = client_ip(
        &headers,
        remote_addr,
        cfg.trust_proxy_headers,
        &cfg.trusted_proxy_cidrs,
    );
    let create_token_valid = tunnel_create_token_valid(&headers, &cfg);
    enforce_tunnel_create_policy(&state, &cfg, &create_ip, create_token_valid).await?;
    enforce_create_rate_limit(
        &state,
        &headers,
        remote_addr,
        cfg.trust_proxy_headers,
        &cfg.trusted_proxy_cidrs,
        cfg.rate_limit_per_ip,
    )
    .await?;
    expire_inactive_tunnels(&state.pool).await?;

    let subdomain = match req.subdomain {
        Some(s) if cfg.allow_custom_subdomain && !cfg.require_random_subdomain => {
            validate_subdomain(&s).map_err(|e| {
                AppError::new(StatusCode::BAD_REQUEST, "invalid_subdomain", e.to_string())
            })?
        }
        _ => random_subdomain(),
    };
    if cfg
        .reserved_subdomains
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(&subdomain))
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "reserved_subdomain",
            "subdomain is reserved",
        ));
    }

    let id = generate_tunnel_id();
    let token = generate_token();
    let token_hash = hash_token(&token);
    let ttl = req.ttl_seconds.unwrap_or(cfg.reserved_ttl_seconds).max(60);
    let url = cfg.public_url(&subdomain).unwrap_or_default();
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let connect_url = format!(
        "{}://{}/api/v1/tunnels/{}/connect",
        if cfg.public_scheme == "https" {
            "wss"
        } else {
            "ws"
        },
        cfg.domain.clone().unwrap_or_default(),
        id
    );

    let result = sqlx::query(
        "INSERT INTO tunnels (id, subdomain, token_hash, status, expires_at, client_ip, client_user_agent, inspector_enabled) \
         VALUES (?1, ?2, ?3, 'reserved', datetime('now', ?4), ?5, ?6, ?7)",
    )
    .bind(&id)
    .bind(&subdomain)
    .bind(&token_hash)
    .bind(format!("+{ttl} seconds"))
    .bind(&create_ip)
    .bind(user_agent.as_deref())
    .bind(cfg.inspector_enabled_default)
    .execute(&state.pool)
    .await;

    if let Err(e) = result {
        let msg = e.to_string();
        if msg.contains("UNIQUE") {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "duplicate_subdomain",
                "subdomain is already reserved or active",
            ));
        }
        return Err(AppError::internal(e));
    }

    add_event(
        &state.pool,
        Some(&id),
        None,
        "tunnel_created",
        Some(&subdomain),
    )
    .await?;

    Ok(Json(ApiResponse::ok(CreateTunnelResponse {
        id,
        token,
        url,
        connect_url,
    })))
}

fn tunnel_create_token_valid(headers: &HeaderMap, cfg: &http_tunnel_common::ServerConfig) -> bool {
    let Some(hash) = cfg.tunnel_create_bearer_token_hash.as_deref() else {
        return false;
    };
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| verify_token(token, hash))
}

async fn enforce_tunnel_create_policy(
    state: &AppState,
    cfg: &http_tunnel_common::ServerConfig,
    create_ip: &str,
    create_token_valid: bool,
) -> Result<()> {
    if !cfg.public_tunnel_create_enabled && !create_token_valid {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "public_tunnel_create_disabled",
            "public tunnel creation is disabled",
        ));
    }
    if cfg.max_active_tunnels_per_ip == 0 || create_token_valid {
        return Ok(());
    }
    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM tunnels \
         WHERE client_ip = ?1 AND status IN ('reserved', 'connected', 'disconnected') \
         AND expires_at > CURRENT_TIMESTAMP",
    )
    .bind(create_ip)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::internal)?;
    if row.get::<i64, _>("count") >= cfg.max_active_tunnels_per_ip as i64 {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_active_tunnels",
            "too many active tunnels for this IP",
        ));
    }
    Ok(())
}

pub async fn get_tunnel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<TunnelRecord>>> {
    let record = load_tunnel(&state.pool, &id).await?;
    Ok(Json(ApiResponse::ok(record)))
}

pub async fn delete_tunnel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<OptionalTokenQuery>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<()>>> {
    require_tunnel_token(&state.pool, &id, &headers, query.token.as_deref()).await?;
    sqlx::query("UPDATE tunnels SET status = 'deleted' WHERE id = ?1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    remove_session_for_tunnel(&state, &id).await;
    add_event(&state.pool, Some(&id), None, "tunnel_deleted", None).await?;
    Ok(Json(ApiResponse::ok(())))
}

pub async fn connect_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ConnectQuery>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    let row = sqlx::query("SELECT id, subdomain, token_hash, enabled, status FROM tunnels WHERE id = ?1 AND status != 'deleted'")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "tunnel_not_found", "tunnel not found"))?;

    let subdomain: String = row.get("subdomain");
    let token_hash: String = row.get("token_hash");
    let enabled: bool = row.get("enabled");
    let status: String = row.get("status");
    if status == "expired" {
        return Err(AppError::new(
            StatusCode::GONE,
            "tunnel_expired",
            "tunnel has expired",
        ));
    }
    if !enabled {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "tunnel_disabled",
            "tunnel is disabled",
        ));
    }
    if !verify_token(&query.token, &token_hash) {
        return Err(AppError::unauthorized());
    }

    let session_id = generate_session_id();
    let cfg = state.config.read().await.clone();
    let source = client_source(&headers, remote_addr, &cfg);
    Ok(
        ws.on_upgrade(move |socket| {
            handle_socket(state, socket, id, subdomain, session_id, source)
        }),
    )
}

async fn handle_socket(
    state: AppState,
    socket: WebSocket,
    tunnel_id: String,
    subdomain: String,
    session_id: String,
    source: ClientSource,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (frame_tx, mut frame_rx) = mpsc::channel::<Frame>(256);
    let last_seen = Arc::new(tokio::sync::RwLock::new(Instant::now()));

    let writer = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            let encoded = match encode_frame(&frame) {
                Ok(encoded) => encoded,
                Err(error) => {
                    tracing::warn!(%error, "failed to encode frame for client");
                    continue;
                }
            };
            if ws_tx.send(Message::Binary(encoded)).await.is_err() {
                break;
            }
        }
    });

    let cfg = state.config.read().await.clone();
    let heartbeat_interval = Duration::from_secs(cfg.heartbeat_interval_seconds.max(1));
    let stale_session_after = Duration::from_secs(cfg.stale_session_seconds.max(1));
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let active_session = ActiveSession {
        tunnel_id: tunnel_id.clone(),
        session_id: session_id.clone(),
        tx: frame_tx.clone(),
        last_seen: last_seen.clone(),
        metrics: SessionRuntimeMetrics::default(),
    };
    let mut registered = false;
    let mut disconnect_reason = "client_disconnect";
    loop {
        tokio::select! {
            _ = heartbeat.tick(), if registered => {
                let seen = *last_seen.read().await;
                if Instant::now().duration_since(seen) > stale_session_after {
                    disconnect_reason = "stale_session";
                    break;
                }
                if frame_tx
                    .send(Frame::new(FrameType::Ping, 0, Vec::new()))
                    .await
                    .is_err()
                {
                    disconnect_reason = "writer_closed";
                    break;
                }
            }
            message = ws_rx.next() => {
                let Some(message) = message else {
                    break;
                };
                match message {
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(Message::Binary(bytes)) => match decode_frame(&bytes) {
                        Ok(frame) => {
                            match frame.frame_type {
                                FrameType::Hello => {
                                    match handle_client_hello(
                                        &state,
                                        &active_session,
                                        &subdomain,
                                        &source,
                                        &frame.payload,
                                    )
                                    .await
                                    {
                                        HelloResult::Accepted => registered = true,
                                        HelloResult::Rejected(reason) => {
                                            disconnect_reason = reason;
                                            break;
                                        }
                                    }
                                    continue;
                                }
                                FrameType::Pong => {
                                    if registered {
                                        mark_session_seen(&state, &session_id, &last_seen).await;
                                    }
                                    continue;
                                }
                                FrameType::Ping => {
                                    let _ = frame_tx
                                        .send(Frame::new(FrameType::Pong, frame.stream_id, Vec::new()))
                                        .await;
                                    continue;
                                }
                                FrameType::ClientSourceUpdate if registered => {
                                    mark_session_seen(&state, &session_id, &last_seen).await;
                                    handle_client_source_update(
                                        &state,
                                        &session_id,
                                        &source,
                                        &frame.payload,
                                    )
                                    .await;
                                    continue;
                                }
                                _ if !registered => {
                                    disconnect_reason = "hello_required";
                                    let _ = frame_tx
                                        .send(Frame::new(FrameType::Goaway, 0, b"hello_required".to_vec()))
                                        .await;
                                    break;
                                }
                                _ => {
                                    mark_session_seen(&state, &session_id, &last_seen).await;
                                }
                            }
                            let pending_tx = state
                                .pending_streams
                                .read()
                                .await
                                .get(&frame.stream_id)
                                .filter(|stream| stream.session_id == session_id)
                                .map(|stream| stream.tx.clone());
                            if let Some(tx) = pending_tx {
                                let _ = tx.send(frame).await;
                            }
                        }
                        Err(error) => tracing::warn!(%error, "dropping invalid client frame"),
                    },
                    _ => {}
                }
            }
        }
    }

    if disconnect_reason == "stale_session" {
        let _ = frame_tx
            .send(Frame::new(FrameType::Goaway, 0, b"stale_session".to_vec()))
            .await;
    }
    let removed_current = state
        .remove_session(&subdomain, &session_id)
        .await
        .is_some();
    if removed_current {
        cancel_pending_streams_for_session(&state, &active_session, disconnect_reason).await;
    }
    let closed_stream_ids = {
        state
            .pending_streams
            .read()
            .await
            .iter()
            .filter(|(_, stream)| stream.tx.is_closed())
            .map(|(stream_id, _)| *stream_id)
            .collect::<Vec<_>>()
    };
    for stream_id in closed_stream_ids {
        state.remove_pending_stream(stream_id).await;
    }
    if registered && removed_current {
        if state.has_active_tunnel_session(&tunnel_id).await {
            let _ = sqlx::query(
                "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = ?1 WHERE id = ?2",
            )
            .bind(disconnect_reason)
            .bind(&session_id)
            .execute(&state.pool)
            .await;
        } else {
            let _ = sqlx::query(
                "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = ?1 WHERE id = ?2; \
                 UPDATE tunnels SET status = 'disconnected', disconnected_at = CURRENT_TIMESTAMP WHERE id = ?3 AND status = 'connected'",
            )
            .bind(disconnect_reason)
            .bind(&session_id)
            .bind(&tunnel_id)
            .execute(&state.pool)
            .await;
        }
        let _ = add_event(
            &state.pool,
            Some(&tunnel_id),
            Some(&session_id),
            "client_disconnected",
            Some(disconnect_reason),
        )
        .await;
    } else if registered {
        let _ = sqlx::query(
            "UPDATE sessions SET disconnected_at = COALESCE(disconnected_at, CURRENT_TIMESTAMP), \
             disconnect_reason = COALESCE(disconnect_reason, ?1) WHERE id = ?2",
        )
        .bind(disconnect_reason)
        .bind(&session_id)
        .execute(&state.pool)
        .await;
    }
    drop(active_session);
    drop(frame_tx);
    let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
}

enum HelloResult {
    Accepted,
    Rejected(&'static str),
}

async fn handle_client_hello(
    state: &AppState,
    session: &ActiveSession,
    subdomain: &str,
    source: &ClientSource,
    payload: &[u8],
) -> HelloResult {
    let hello = match decode_payload::<Hello>(payload) {
        Ok(hello) => hello,
        Err(error) => {
            tracing::warn!(%error, "invalid client hello");
            let _ = send_hello_ack(&session.tx, false, Some("invalid hello"), None).await;
            let _ = session
                .tx
                .send(Frame::new(FrameType::Goaway, 0, b"invalid_hello".to_vec()))
                .await;
            return HelloResult::Rejected("invalid_hello");
        }
    };
    if hello
        .protocol_version
        .is_some_and(|version| version != PROTOCOL_VERSION)
    {
        let _ = send_hello_ack(
            &session.tx,
            false,
            Some("unsupported protocol version"),
            None,
        )
        .await;
        let _ = session
            .tx
            .send(Frame::new(
                FrameType::Goaway,
                0,
                b"unsupported_protocol_version".to_vec(),
            ))
            .await;
        return HelloResult::Rejected("unsupported_protocol_version");
    }

    let reconnect_accepted = match hello.reconnect_token.as_deref() {
        Some(token) => verify_reconnect_token(state, token, &session.tunnel_id, subdomain).await,
        None => false,
    };
    let capabilities = serde_json::to_string(&hello.capabilities).unwrap_or_else(|_| "[]".into());
    let cfg = state.config.read().await.clone();
    let resolved_source = resolve_client_source(&cfg, source, hello.client_source.as_ref());
    let session_pool_policy = effective_session_pool_policy(&cfg);
    let registration = state
        .register_session(subdomain, session.clone(), session_pool_policy)
        .await;
    if registration.rejected {
        let _ = send_hello_ack(&session.tx, false, Some("duplicate session"), None).await;
        let _ = session
            .tx
            .send(Frame::new(
                FrameType::Goaway,
                0,
                b"duplicate_session".to_vec(),
            ))
            .await;
        let _ = add_event(
            &state.pool,
            Some(&session.tunnel_id),
            None,
            "client_duplicate_rejected",
            Some(subdomain),
        )
        .await;
        return HelloResult::Rejected("duplicate_session");
    }
    for old_session in registration.replaced {
        cancel_pending_streams_for_session(state, &old_session, "duplicate_replaced").await;
        let _ = old_session
            .tx
            .send(Frame::new(
                FrameType::Goaway,
                0,
                b"duplicate_replaced".to_vec(),
            ))
            .await;
        let _ = sqlx::query(
            "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = 'duplicate_replaced' WHERE id = ?1 AND disconnected_at IS NULL",
        )
        .bind(&old_session.session_id)
        .execute(&state.pool)
        .await;
        let _ = add_event(
            &state.pool,
            Some(&old_session.tunnel_id),
            Some(&old_session.session_id),
            "client_duplicate_replaced",
            Some(subdomain),
        )
        .await;
    }
    let _ = sqlx::query(
        "INSERT OR IGNORE INTO sessions \
            (id, tunnel_id, remote_addr, client_reported_ip, client_reported_ip_updated_at, client_country_source, client_country_code, client_country) \
         VALUES (?1, ?2, ?3, ?4, CASE WHEN ?4 IS NULL THEN NULL ELSE CURRENT_TIMESTAMP END, ?5, ?6, ?7); \
         UPDATE sessions SET client_version = ?8, client_capabilities = ?9, last_seen_at = CURRENT_TIMESTAMP, remote_addr = ?3, \
             client_reported_ip = ?4, \
             client_reported_ip_updated_at = CASE WHEN ?4 IS NULL THEN client_reported_ip_updated_at ELSE CURRENT_TIMESTAMP END, \
             client_country_source = ?5, client_country_code = ?6, client_country = ?7 \
         WHERE id = ?1; \
         UPDATE tunnels SET status = 'connected', connected_at = CURRENT_TIMESTAMP, disconnected_at = NULL, expires_at = datetime('now', ?10) WHERE id = ?2",
    )
    .bind(&session.session_id)
    .bind(&session.tunnel_id)
    .bind(&source.ip)
    .bind(resolved_source.reported_ip.as_deref())
    .bind(resolved_source.country_source)
    .bind(resolved_source.country_code.as_deref())
    .bind(resolved_source.country.as_deref())
    .bind(hello.client_version.as_deref())
    .bind(capabilities)
    .bind(format!("+{} seconds", cfg.tunnel_ttl_seconds))
    .execute(&state.pool)
    .await;
    if hello.reconnect_token.is_some() {
        let _ = add_event(
            &state.pool,
            Some(&session.tunnel_id),
            Some(&session.session_id),
            if reconnect_accepted {
                "client_reconnect_token_accepted"
            } else {
                "client_reconnect_token_rejected"
            },
            None,
        )
        .await;
    }
    let _ = add_event(
        &state.pool,
        Some(&session.tunnel_id),
        Some(&session.session_id),
        "client_connected",
        Some(subdomain),
    )
    .await;
    let reconnect_token =
        issue_reconnect_token(state, &session.tunnel_id, subdomain, &session.session_id).await;
    let _ = add_event(
        &state.pool,
        Some(&session.tunnel_id),
        Some(&session.session_id),
        "client_hello",
        hello.client_version.as_deref(),
    )
    .await;
    let _ = send_hello_ack(&session.tx, true, None, reconnect_token).await;
    HelloResult::Accepted
}

async fn handle_client_source_update(
    state: &AppState,
    session_id: &str,
    source: &ClientSource,
    payload: &[u8],
) {
    let report = match decode_payload::<ClientSourceReport>(payload) {
        Ok(report) => report,
        Err(error) => {
            tracing::debug!(%error, "invalid client source update");
            return;
        }
    };
    let cfg = state.config.read().await.clone();
    let resolved_source = resolve_client_source(&cfg, source, Some(&report));
    let result = sqlx::query(
        "UPDATE sessions SET \
             client_reported_ip = ?1, \
             client_reported_ip_updated_at = CASE WHEN ?1 IS NULL THEN client_reported_ip_updated_at ELSE CURRENT_TIMESTAMP END, \
             client_country_source = ?2, \
             client_country_code = ?3, \
             client_country = ?4, \
             last_seen_at = CURRENT_TIMESTAMP \
         WHERE id = ?5",
    )
    .bind(resolved_source.reported_ip.as_deref())
    .bind(resolved_source.country_source)
    .bind(resolved_source.country_code.as_deref())
    .bind(resolved_source.country.as_deref())
    .bind(session_id)
    .execute(&state.pool)
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "failed to update client source");
    }
}

async fn send_hello_ack(
    frame_tx: &mpsc::Sender<Frame>,
    accepted: bool,
    message: Option<&str>,
    reconnect_token: Option<String>,
) -> std::result::Result<(), mpsc::error::SendError<Frame>> {
    frame_tx
        .send(Frame::new(
            FrameType::HelloAck,
            0,
            encode_payload(&HelloAck {
                accepted,
                message: message.map(ToString::to_string),
                reconnect_token,
                capabilities: if accepted {
                    vec![CLIENT_SOURCE_UPDATE_CAPABILITY.to_string()]
                } else {
                    Vec::new()
                },
            })
            .unwrap_or_default(),
        ))
        .await
}

#[derive(Debug, Serialize, Deserialize)]
struct ReconnectTokenPayload {
    tunnel_id: String,
    subdomain: String,
    session_id: String,
    expires_at: u64,
}

async fn issue_reconnect_token(
    state: &AppState,
    tunnel_id: &str,
    subdomain: &str,
    session_id: &str,
) -> Option<String> {
    let cfg = state.config.read().await;
    let secret = cfg
        .reconnect_token_secret
        .as_deref()
        .or(cfg.admin_session_secret.as_deref())?;
    let expires_at = unix_now().saturating_add(300);
    let payload = ReconnectTokenPayload {
        tunnel_id: tunnel_id.to_string(),
        subdomain: subdomain.to_string(),
        session_id: session_id.to_string(),
        expires_at,
    };
    let raw = serde_json::to_vec(&payload).ok()?;
    let encoded = URL_SAFE_NO_PAD.encode(raw);
    let signature = sign_reconnect_payload(secret, &encoded)?;
    Some(format!("{encoded}.{signature}"))
}

async fn verify_reconnect_token(
    state: &AppState,
    token: &str,
    tunnel_id: &str,
    subdomain: &str,
) -> bool {
    let cfg = state.config.read().await;
    let Some(secret) = cfg
        .reconnect_token_secret
        .as_deref()
        .or(cfg.admin_session_secret.as_deref())
    else {
        return false;
    };
    let Some((encoded, signature)) = token.rsplit_once('.') else {
        return false;
    };
    if !verify_reconnect_signature(secret, encoded, signature) {
        return false;
    }
    let Ok(raw) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<ReconnectTokenPayload>(&raw) else {
        return false;
    };
    payload.expires_at > unix_now()
        && payload.tunnel_id == tunnel_id
        && payload.subdomain == subdomain
}

type HmacSha256 = Hmac<Sha256>;

fn sign_reconnect_payload(secret: &str, encoded_payload: &str) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(encoded_payload.as_bytes());
    Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn verify_reconnect_signature(secret: &str, encoded_payload: &str, signature: &str) -> bool {
    let Ok(signature_bytes) = URL_SAFE_NO_PAD.decode(signature) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(encoded_payload.as_bytes());
    mac.verify_slice(&signature_bytes).is_ok()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

async fn mark_session_seen(
    state: &AppState,
    session_id: &str,
    last_seen: &tokio::sync::RwLock<Instant>,
) {
    *last_seen.write().await = Instant::now();
    let _ = sqlx::query("UPDATE sessions SET last_seen_at = CURRENT_TIMESTAMP WHERE id = ?1")
        .bind(session_id)
        .execute(&state.pool)
        .await;
}

pub async fn admin_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TunnelListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_tunnel_rows(&state.pool, &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_TUNNEL_LIMIT);
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, subdomain, status, enabled, created_at, expires_at, access_policy, access_token_hash, \
                access_username, allowed_methods, blocked_path_prefixes, inspector_enabled, rate_limit_per_minute \
         FROM tunnels WHERE status != 'deleted'",
    );
    if let Some(status) = non_empty(query.status.as_deref()) {
        builder.push(" AND status = ").push_bind(status.to_string());
    }
    if let Some(subdomain) = non_empty(query.subdomain.as_deref()) {
        builder
            .push(" AND subdomain LIKE ")
            .push_bind(format!("%{subdomain}%"));
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR subdomain LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    push_limit_offset(
        &mut builder,
        "created_at",
        query.limit,
        query.offset,
        DEFAULT_TUNNEL_LIMIT,
        MAX_PAGE_LIMIT,
    );
    let rows = builder
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::internal)?;
    list_response(
        rows.into_iter().map(row_to_tunnel).collect::<Vec<_>>(),
        total_count,
        limit,
        offset,
    )
}

pub async fn admin_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<TunnelRecord>>> {
    require_admin(&state, &headers).await?;
    Ok(Json(ApiResponse::ok(load_tunnel(&state.pool, &id).await?)))
}

pub async fn admin_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<TunnelDetail>>> {
    require_admin(&state, &headers).await?;
    let tunnel = load_tunnel(&state.pool, &id).await?;
    let runtime_session_metrics = state
        .sessions_for_tunnel(&id)
        .await
        .into_iter()
        .map(|session| (session.session_id.clone(), session.runtime_metrics()))
        .collect::<HashMap<_, _>>();
    let active_session = load_latest_session(&state.pool, &id, &runtime_session_metrics).await?;
    let active_sessions = load_active_sessions(&state.pool, &id, &runtime_session_metrics).await?;
    let request_count = count_requests(&state.pool, &id, false).await?;
    let error_count = count_requests(&state.pool, &id, true).await?;
    let recent_requests = request_rows(
        &state.pool,
        Some(&id),
        RequestListQuery {
            limit: Some(10),
            ..RequestListQuery::default()
        },
    )
    .await?;
    let recent_events = event_rows(
        &state.pool,
        Some(&id),
        EventListQuery {
            limit: Some(10),
            ..EventListQuery::default()
        },
    )
    .await?;
    let last_error = last_error_row(&state.pool, &id).await?;
    Ok(Json(ApiResponse::ok(TunnelDetail {
        tunnel,
        active_session,
        active_sessions,
        request_count,
        error_count,
        recent_requests,
        recent_events,
        last_error,
    })))
}

pub async fn admin_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(req): Json<TunnelPatchRequest>,
) -> Result<Json<ApiResponse<TunnelRecord>>> {
    let actor = require_admin_write(&state, &headers).await?;
    ensure_admin_tunnel_exists(&state, &headers, remote_addr, &actor, "tunnel_patch", &id).await?;
    if let Some(ttl_seconds) = req.ttl_seconds {
        if ttl_seconds < 60 {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_ttl",
                "ttl_seconds must be at least 60",
            ));
        }
        sqlx::query(
            "UPDATE tunnels SET expires_at = datetime('now', ?1) WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(format!("+{ttl_seconds} seconds"))
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if req.expire_now.unwrap_or(false) {
        remove_session_for_tunnel(&state, &id).await;
        sqlx::query(
            "UPDATE tunnels SET status = 'expired', expires_at = CURRENT_TIMESTAMP, disconnected_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status != 'deleted'",
        )
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(enabled) = req.enabled {
        if enabled {
            sqlx::query(
                "UPDATE tunnels SET enabled = TRUE, status = CASE WHEN status = 'disabled' THEN 'disconnected' ELSE status END WHERE id = ?1 AND status != 'deleted'",
            )
            .bind(&id)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
        } else {
            remove_session_for_tunnel(&state, &id).await;
            sqlx::query(
                "UPDATE tunnels SET enabled = FALSE, status = 'disabled', disconnected_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status != 'deleted'",
            )
            .bind(&id)
            .execute(&state.pool)
            .await
                .map_err(AppError::internal)?;
        }
    }
    if let Some(access_policy) = req.access_policy.as_deref() {
        let access_policy = validate_access_policy(access_policy)?;
        if access_policy == "public" {
            sqlx::query(
                "UPDATE tunnels SET access_policy = 'public', access_token_hash = NULL, access_username = NULL, access_password_hash = NULL \
                 WHERE id = ?1 AND status != 'deleted'",
            )
            .bind(&id)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
        } else {
            sqlx::query(
                "UPDATE tunnels SET access_policy = ?1 WHERE id = ?2 AND status != 'deleted'",
            )
            .bind(access_policy)
            .bind(&id)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
        }
    }
    if let Some(access_token) = non_empty(req.access_token.as_deref()) {
        sqlx::query(
            "UPDATE tunnels SET access_token_hash = ?1, access_policy = CASE WHEN access_policy = 'public' THEN 'bearer' ELSE access_policy END \
             WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(hash_token(access_token))
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(access_username) = req.access_username.as_deref() {
        let username = non_empty(Some(access_username)).ok_or_else(|| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_access_username",
                "access_username must not be empty",
            )
        })?;
        sqlx::query(
            "UPDATE tunnels SET access_username = ?1, access_policy = CASE WHEN access_policy = 'public' THEN 'basic' ELSE access_policy END \
             WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(username)
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(access_password) = non_empty(req.access_password.as_deref()) {
        let password_hash = hash_password(access_password).map_err(AppError::internal)?;
        sqlx::query(
            "UPDATE tunnels SET access_password_hash = ?1, access_policy = CASE WHEN access_policy = 'public' THEN 'basic' ELSE access_policy END \
             WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(password_hash)
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(methods) = req.allowed_methods.as_ref() {
        let methods = normalize_methods(methods)?;
        sqlx::query(
            "UPDATE tunnels SET allowed_methods = ?1 WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(optional_json_array(&methods))
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(prefixes) = req.blocked_path_prefixes.as_ref() {
        let prefixes = normalize_path_prefixes(prefixes)?;
        sqlx::query(
            "UPDATE tunnels SET blocked_path_prefixes = ?1 WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(optional_json_array(&prefixes))
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(inspector_enabled) = req.inspector_enabled {
        sqlx::query(
            "UPDATE tunnels SET inspector_enabled = ?1 WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(inspector_enabled)
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    if let Some(rate_limit) = req.rate_limit_per_minute {
        let rate_limit_value = if rate_limit == 0 {
            None
        } else {
            Some(i64::try_from(rate_limit).unwrap_or(i64::MAX))
        };
        sqlx::query(
            "UPDATE tunnels SET rate_limit_per_minute = ?1 WHERE id = ?2 AND status != 'deleted'",
        )
        .bind(rate_limit_value)
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    }
    let detail = serde_json::to_string(&serde_json::json!({
        "ttl_seconds": req.ttl_seconds,
        "expire_now": req.expire_now.unwrap_or(false),
        "enabled": req.enabled,
        "access_policy": req.access_policy,
        "access_token_updated": req.access_token.as_deref().is_some_and(|value| !value.trim().is_empty()),
        "access_username": req.access_username,
        "access_password_updated": req.access_password.as_deref().is_some_and(|value| !value.trim().is_empty()),
        "allowed_methods": req.allowed_methods,
        "blocked_path_prefixes": req.blocked_path_prefixes,
        "inspector_enabled": req.inspector_enabled,
        "rate_limit_per_minute": req.rate_limit_per_minute,
    }))
    .ok();
    add_event(
        &state.pool,
        Some(&id),
        None,
        "admin_tunnel_patched",
        detail.as_deref(),
    )
    .await?;
    record_tunnel_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_patch",
        &id,
        detail.as_deref(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(load_tunnel(&state.pool, &id).await?)))
}

pub async fn admin_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>> {
    let actor = require_admin_write(&state, &headers).await?;
    ensure_admin_tunnel_exists(&state, &headers, remote_addr, &actor, "tunnel_delete", &id).await?;
    let response = delete_by_id(&state, &id).await?;
    record_tunnel_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_delete",
        &id,
        None,
    )
    .await?;
    Ok(response)
}

pub async fn admin_disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>> {
    let actor = require_admin_write(&state, &headers).await?;
    ensure_admin_tunnel_exists(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_disconnect",
        &id,
    )
    .await?;
    remove_session_for_tunnel(&state, &id).await;
    sqlx::query("UPDATE tunnels SET status = 'disconnected', disconnected_at = CURRENT_TIMESTAMP WHERE id = ?1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    add_event(&state.pool, Some(&id), None, "admin_disconnect", None).await?;
    record_tunnel_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_disconnect",
        &id,
        None,
    )
    .await?;
    Ok(Json(ApiResponse::ok(())))
}

pub async fn admin_disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>> {
    let actor = require_admin_write(&state, &headers).await?;
    ensure_admin_tunnel_exists(&state, &headers, remote_addr, &actor, "tunnel_disable", &id)
        .await?;
    sqlx::query("UPDATE tunnels SET enabled = FALSE, status = 'disabled' WHERE id = ?1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    remove_session_for_tunnel(&state, &id).await;
    add_event(&state.pool, Some(&id), None, "admin_disable", None).await?;
    record_tunnel_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_disable",
        &id,
        None,
    )
    .await?;
    Ok(Json(ApiResponse::ok(())))
}

pub async fn admin_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>> {
    let actor = require_admin_write(&state, &headers).await?;
    ensure_admin_tunnel_exists(&state, &headers, remote_addr, &actor, "tunnel_enable", &id).await?;
    sqlx::query("UPDATE tunnels SET enabled = TRUE, status = 'disconnected' WHERE id = ?1 AND status = 'disabled'")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    add_event(&state.pool, Some(&id), None, "admin_enable", None).await?;
    record_tunnel_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_enable",
        &id,
        None,
    )
    .await?;
    Ok(Json(ApiResponse::ok(())))
}

pub async fn admin_rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<RotateTokenResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let token = generate_token();
    let token_hash = hash_token(&token);
    let result =
        sqlx::query("UPDATE tunnels SET token_hash = ?1 WHERE id = ?2 AND status != 'deleted'")
            .bind(&token_hash)
            .bind(&id)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
    if result.rows_affected() == 0 {
        record_tunnel_failure_audit(
            &state,
            &headers,
            remote_addr,
            &actor,
            "tunnel_token_rotate",
            &id,
            "tunnel not found",
        )
        .await?;
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            "tunnel_not_found",
            "tunnel not found",
        ));
    }
    remove_session_for_tunnel(&state, &id).await;
    add_event(&state.pool, Some(&id), None, "admin_token_rotated", None).await?;
    record_tunnel_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "tunnel_token_rotate",
        &id,
        None,
    )
    .await?;
    Ok(Json(ApiResponse::ok(RotateTokenResponse { token })))
}

pub async fn admin_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<RequestListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_request_rows(&state.pool, Some(&id), &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_LOG_LIMIT);
    let rows = request_rows(&state.pool, Some(&id), query).await?;
    list_response(rows, total_count, limit, offset)
}

pub async fn admin_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<EventListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_event_rows(&state.pool, Some(&id), &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_LOG_LIMIT);
    let rows = event_rows(&state.pool, Some(&id), query).await?;
    list_response(rows, total_count, limit, offset)
}

async fn delete_by_id(state: &AppState, id: &str) -> Result<Json<ApiResponse<()>>> {
    sqlx::query("UPDATE tunnels SET status = 'deleted' WHERE id = ?1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    remove_session_for_tunnel(state, id).await;
    add_event(&state.pool, Some(id), None, "admin_delete", None).await?;
    Ok(Json(ApiResponse::ok(())))
}

async fn ensure_admin_tunnel_exists(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    actor: &str,
    action: &str,
    tunnel_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM tunnels WHERE id = ?1 AND status != 'deleted'")
        .bind(tunnel_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?
        .is_some();
    if exists {
        return Ok(());
    }
    record_tunnel_failure_audit(
        state,
        headers,
        remote_addr,
        actor,
        action,
        tunnel_id,
        "tunnel not found",
    )
    .await?;
    Err(AppError::new(
        StatusCode::NOT_FOUND,
        "tunnel_not_found",
        "tunnel not found",
    ))
}

async fn record_tunnel_audit(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    actor: &str,
    action: &str,
    tunnel_id: &str,
    detail: Option<&str>,
) -> Result<()> {
    record_admin_audit(
        state,
        headers,
        remote_addr,
        AuditLog {
            actor_token: Some(actor),
            action,
            target_type: Some("tunnel"),
            target_id: Some(tunnel_id),
            result: "success",
            detail,
        },
    )
    .await
}

async fn record_tunnel_failure_audit(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    actor: &str,
    action: &str,
    tunnel_id: &str,
    detail: &str,
) -> Result<()> {
    record_admin_audit(
        state,
        headers,
        remote_addr,
        AuditLog {
            actor_token: Some(actor),
            action,
            target_type: Some("tunnel"),
            target_id: Some(tunnel_id),
            result: "failure",
            detail: Some(detail),
        },
    )
    .await
}

async fn load_tunnel(pool: &SqlitePool, id: &str) -> Result<TunnelRecord> {
    let row = sqlx::query(
        "SELECT id, subdomain, status, enabled, created_at, expires_at, access_policy, access_token_hash, \
                access_username, allowed_methods, blocked_path_prefixes, inspector_enabled, rate_limit_per_minute \
         FROM tunnels WHERE id = ?1 AND status != 'deleted'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "tunnel_not_found", "tunnel not found"))?;
    Ok(row_to_tunnel(row))
}

async fn load_latest_session(
    pool: &SqlitePool,
    tunnel_id: &str,
    runtime_session_metrics: &HashMap<String, RuntimeSessionMetricsSnapshot>,
) -> Result<Option<SessionRecord>> {
    let row = sqlx::query(
        "SELECT id, connected_at, disconnected_at, disconnect_reason, last_seen_at, client_version, client_capabilities, remote_addr \
         FROM sessions WHERE tunnel_id = ?1 \
         ORDER BY connected_at DESC LIMIT 1",
    )
    .bind(tunnel_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::internal)?;
    Ok(row.map(|row| row_to_session(row, runtime_session_metrics)))
}

async fn load_active_sessions(
    pool: &SqlitePool,
    tunnel_id: &str,
    runtime_session_metrics: &HashMap<String, RuntimeSessionMetricsSnapshot>,
) -> Result<Vec<SessionRecord>> {
    if runtime_session_metrics.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, connected_at, disconnected_at, disconnect_reason, last_seen_at, client_version, client_capabilities, remote_addr \
         FROM sessions WHERE tunnel_id = ",
    );
    builder.push_bind(tunnel_id.to_string());
    builder.push(" AND id IN (");
    let mut separated = builder.separated(", ");
    for id in runtime_session_metrics.keys() {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(") ORDER BY connected_at DESC");
    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(|row| row_to_session(row, runtime_session_metrics))
        .collect())
}

fn row_to_session(
    row: sqlx::sqlite::SqliteRow,
    runtime_session_metrics: &HashMap<String, RuntimeSessionMetricsSnapshot>,
) -> SessionRecord {
    let id = row.get::<String, _>("id");
    let runtime_metrics = runtime_session_metrics.get(&id).copied();
    SessionRecord {
        runtime_active: runtime_metrics.is_some(),
        runtime_active_streams: runtime_metrics.map(|metrics| metrics.active_streams),
        runtime_bytes_in: runtime_metrics.map(|metrics| metrics.bytes_in),
        runtime_bytes_out: runtime_metrics.map(|metrics| metrics.bytes_out),
        runtime_selected_count: runtime_metrics.map(|metrics| metrics.selected_count),
        runtime_last_selected_unix_ms: runtime_metrics
            .and_then(|metrics| metrics.last_selected_unix_ms),
        id,
        connected_at: row.get::<String, _>("connected_at"),
        disconnected_at: row
            .try_get::<Option<String>, _>("disconnected_at")
            .ok()
            .flatten(),
        disconnect_reason: row
            .try_get::<Option<String>, _>("disconnect_reason")
            .ok()
            .flatten(),
        last_seen_at: row.get::<String, _>("last_seen_at"),
        client_version: row
            .try_get::<Option<String>, _>("client_version")
            .ok()
            .flatten(),
        client_capabilities: row
            .try_get::<Option<String>, _>("client_capabilities")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default(),
        remote_addr: row
            .try_get::<Option<String>, _>("remote_addr")
            .ok()
            .flatten(),
    }
}

async fn count_requests(pool: &SqlitePool, tunnel_id: &str, errors_only: bool) -> Result<i64> {
    let sql = if errors_only {
        "SELECT COUNT(*) AS count FROM request_logs WHERE tunnel_id = ?1 AND error IS NOT NULL"
    } else {
        "SELECT COUNT(*) AS count FROM request_logs WHERE tunnel_id = ?1"
    };
    let row = sqlx::query(sql)
        .bind(tunnel_id)
        .fetch_one(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(row.get::<i64, _>("count"))
}

async fn count_tunnel_rows(pool: &SqlitePool, query: &TunnelListQuery) -> Result<i64> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT COUNT(*) AS count FROM tunnels WHERE status != 'deleted'",
    );
    if let Some(status) = non_empty(query.status.as_deref()) {
        builder.push(" AND status = ").push_bind(status.to_string());
    }
    if let Some(subdomain) = non_empty(query.subdomain.as_deref()) {
        builder
            .push(" AND subdomain LIKE ")
            .push_bind(format!("%{subdomain}%"));
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR subdomain LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    builder
        .build()
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>("count"))
        .map_err(AppError::internal)
}

async fn last_error_row(pool: &SqlitePool, tunnel_id: &str) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query(
        "SELECT id, tunnel_id, session_id, request_type, method, path, host, remote_ip, user_agent, status, started_at, completed_at, duration_ms, bytes_in, bytes_out, error, ws_message_count, ws_close_code, ws_close_reason, replay_of \
         FROM request_logs \
         WHERE tunnel_id = ?1 AND error IS NOT NULL \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(tunnel_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::internal)?;
    Ok(row.map(request_row_to_json))
}

pub(crate) async fn count_request_rows(
    pool: &SqlitePool,
    tunnel_id: Option<&str>,
    query: &RequestListQuery,
) -> Result<i64> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT COUNT(*) AS count FROM request_logs WHERE 1 = 1",
    );
    if let Some(tunnel_id) = tunnel_id {
        builder
            .push(" AND tunnel_id = ")
            .push_bind(tunnel_id.to_string());
    }
    if let Some(status) = query.status {
        builder.push(" AND status = ").push_bind(status);
    }
    if query.error_only.unwrap_or(false) {
        builder.push(" AND error IS NOT NULL");
    }
    if let Some(request_type) = non_empty(query.request_type.as_deref()) {
        builder
            .push(" AND request_type = ")
            .push_bind(request_type.to_string());
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (tunnel_id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR method LIKE ")
            .push_bind(pattern.clone())
            .push(" OR path LIKE ")
            .push_bind(pattern.clone())
            .push(" OR host LIKE ")
            .push_bind(pattern.clone())
            .push(" OR remote_ip LIKE ")
            .push_bind(pattern.clone())
            .push(" OR error LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    builder
        .build()
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>("count"))
        .map_err(AppError::internal)
}

pub(crate) async fn request_rows(
    pool: &SqlitePool,
    tunnel_id: Option<&str>,
    query: RequestListQuery,
) -> Result<Vec<serde_json::Value>> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, tunnel_id, session_id, request_type, method, path, host, remote_ip, user_agent, status, started_at, completed_at, duration_ms, bytes_in, bytes_out, error, ws_message_count, ws_close_code, ws_close_reason, replay_of FROM request_logs WHERE 1 = 1",
    );
    if let Some(tunnel_id) = tunnel_id {
        builder
            .push(" AND tunnel_id = ")
            .push_bind(tunnel_id.to_string());
    }
    if let Some(status) = query.status {
        builder.push(" AND status = ").push_bind(status);
    }
    if query.error_only.unwrap_or(false) {
        builder.push(" AND error IS NOT NULL");
    }
    if let Some(request_type) = non_empty(query.request_type.as_deref()) {
        builder
            .push(" AND request_type = ")
            .push_bind(request_type.to_string());
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (tunnel_id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR method LIKE ")
            .push_bind(pattern.clone())
            .push(" OR path LIKE ")
            .push_bind(pattern.clone())
            .push(" OR host LIKE ")
            .push_bind(pattern.clone())
            .push(" OR remote_ip LIKE ")
            .push_bind(pattern.clone())
            .push(" OR error LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    push_limit_offset(
        &mut builder,
        "started_at",
        query.limit,
        query.offset,
        DEFAULT_LOG_LIMIT,
        if query.all.unwrap_or(false) {
            MAX_EXPORT_LIMIT
        } else {
            MAX_PAGE_LIMIT
        },
    );
    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(rows.into_iter().map(request_row_to_json).collect())
}

pub(crate) async fn count_event_rows(
    pool: &SqlitePool,
    tunnel_id: Option<&str>,
    query: &EventListQuery,
) -> Result<i64> {
    let mut builder =
        sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT COUNT(*) AS count FROM events WHERE 1 = 1");
    if let Some(tunnel_id) = tunnel_id {
        builder
            .push(" AND tunnel_id = ")
            .push_bind(tunnel_id.to_string());
    }
    if let Some(kind) = non_empty(query.kind.as_deref()) {
        builder.push(" AND kind = ").push_bind(kind.to_string());
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (kind LIKE ")
            .push_bind(pattern.clone())
            .push(" OR message LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    builder
        .build()
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>("count"))
        .map_err(AppError::internal)
}

pub(crate) async fn event_rows(
    pool: &SqlitePool,
    tunnel_id: Option<&str>,
    query: EventListQuery,
) -> Result<Vec<serde_json::Value>> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, tunnel_id, session_id, kind, message, created_at FROM events WHERE 1 = 1",
    );
    if let Some(tunnel_id) = tunnel_id {
        builder
            .push(" AND tunnel_id = ")
            .push_bind(tunnel_id.to_string());
    }
    if let Some(kind) = non_empty(query.kind.as_deref()) {
        builder.push(" AND kind = ").push_bind(kind.to_string());
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (kind LIKE ")
            .push_bind(pattern.clone())
            .push(" OR message LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    push_limit_offset(
        &mut builder,
        "created_at",
        query.limit,
        query.offset,
        DEFAULT_LOG_LIMIT,
        MAX_PAGE_LIMIT,
    );
    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(rows.into_iter().map(event_row_to_json).collect())
}

pub(crate) fn request_row_to_json(row: sqlx::sqlite::SqliteRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.get::<String, _>("id"),
        "request_id": row.get::<String, _>("id"),
        "tunnel_id": row.get::<String, _>("tunnel_id"),
        "session_id": row.try_get::<Option<String>, _>("session_id").ok().flatten(),
        "type": row.try_get::<String, _>("request_type").unwrap_or_else(|_| "http".to_string()),
        "method": row.get::<String, _>("method"),
        "path": row.get::<String, _>("path"),
        "host": row.try_get::<Option<String>, _>("host").ok().flatten(),
        "remote_ip": row.try_get::<Option<String>, _>("remote_ip").ok().flatten(),
        "user_agent": row.try_get::<Option<String>, _>("user_agent").ok().flatten(),
        "status": row.try_get::<Option<i64>, _>("status").ok().flatten(),
        "started_at": row.get::<String, _>("started_at"),
        "completed_at": row.try_get::<Option<String>, _>("completed_at").ok().flatten(),
        "duration_ms": row.try_get::<Option<i64>, _>("duration_ms").ok().flatten(),
        "bytes_in": row.try_get::<Option<i64>, _>("bytes_in").ok().flatten(),
        "bytes_out": row.try_get::<Option<i64>, _>("bytes_out").ok().flatten(),
        "error": row.try_get::<Option<String>, _>("error").ok().flatten(),
        "ws_message_count": row.try_get::<Option<i64>, _>("ws_message_count").ok().flatten(),
        "ws_close_code": row.try_get::<Option<i64>, _>("ws_close_code").ok().flatten(),
        "ws_close_reason": row.try_get::<Option<String>, _>("ws_close_reason").ok().flatten(),
        "replay_of": row.try_get::<Option<String>, _>("replay_of").ok().flatten(),
    })
}

pub(crate) fn event_row_to_json(row: sqlx::sqlite::SqliteRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.get::<String, _>("id"),
        "tunnel_id": row.try_get::<Option<String>, _>("tunnel_id").ok().flatten(),
        "session_id": row.try_get::<Option<String>, _>("session_id").ok().flatten(),
        "kind": row.get::<String, _>("kind"),
        "message": row.try_get::<Option<String>, _>("message").ok().flatten(),
        "created_at": row.get::<String, _>("created_at"),
    })
}

fn row_to_tunnel(row: sqlx::sqlite::SqliteRow) -> TunnelRecord {
    TunnelRecord {
        id: row.get("id"),
        subdomain: row.get("subdomain"),
        status: row.get("status"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        access_policy: row
            .try_get::<String, _>("access_policy")
            .unwrap_or_else(|_| "public".to_string()),
        access_token_configured: row
            .try_get::<Option<String>, _>("access_token_hash")
            .ok()
            .flatten()
            .is_some_and(|hash| !hash.is_empty()),
        access_username: row
            .try_get::<Option<String>, _>("access_username")
            .ok()
            .flatten(),
        allowed_methods: json_string_array(
            row.try_get::<Option<String>, _>("allowed_methods")
                .ok()
                .flatten()
                .as_deref(),
        ),
        blocked_path_prefixes: json_string_array(
            row.try_get::<Option<String>, _>("blocked_path_prefixes")
                .ok()
                .flatten()
                .as_deref(),
        ),
        inspector_enabled: row.try_get::<bool, _>("inspector_enabled").unwrap_or(false),
        rate_limit_per_minute: row
            .try_get::<Option<i64>, _>("rate_limit_per_minute")
            .ok()
            .flatten(),
    }
}

fn json_string_array(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
}

async fn remove_session_for_tunnel(state: &AppState, tunnel_id: &str) {
    let sessions = state.remove_sessions_for_tunnel(tunnel_id).await;
    for session in sessions {
        let _ = session
            .tx
            .send(Frame::new(
                FrameType::Goaway,
                0,
                b"admin_disconnect".to_vec(),
            ))
            .await;
        drain_pending_streams_for_session(state, &session, "admin_disconnect").await;
        let _ = sqlx::query(
            "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = 'admin_disconnect' WHERE id = ?1 AND disconnected_at IS NULL",
        )
        .bind(&session.session_id)
        .execute(&state.pool)
        .await;
    }
}

async fn drain_pending_streams_for_session(
    state: &AppState,
    session: &ActiveSession,
    reason: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        if state
            .pending_stream_count_for_session(&session.tunnel_id, &session.session_id)
            .await
            == 0
        {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            cancel_pending_streams_for_session(state, session, reason).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn cancel_pending_streams_for_session(
    state: &AppState,
    session: &ActiveSession,
    reason: &str,
) {
    let streams = state
        .remove_pending_streams_for_session(&session.tunnel_id, &session.session_id)
        .await;
    for (stream_id, _) in streams {
        let _ = session
            .tx
            .send(Frame::new(
                FrameType::Cancel,
                stream_id,
                reason.as_bytes().to_vec(),
            ))
            .await;
    }
}

#[allow(dead_code)]
async fn cancel_pending_streams_for_tunnel(state: &AppState, tunnel_id: &str, reason: &str) {
    let sessions = state.sessions_for_tunnel(tunnel_id).await;
    for session in sessions {
        cancel_pending_streams_for_session(state, &session, reason).await;
    }
}

async fn add_event(
    pool: &SqlitePool,
    tunnel_id: Option<&str>,
    session_id: Option<&str>,
    kind: &str,
    message: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO events (id, tunnel_id, session_id, kind, message) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(generate_event_id())
    .bind(tunnel_id)
    .bind(session_id)
    .bind(kind)
    .bind(message)
    .execute(pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

fn random_subdomain() -> String {
    let id = generate_tunnel_id();
    format!("t{}", &id[id.len() - 10..])
}

async fn enforce_create_rate_limit(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_proxy_headers: bool,
    trusted_proxy_cidrs: &[String],
    limit_per_ip: u64,
) -> Result<()> {
    if limit_per_ip == 0 {
        return Ok(());
    }
    let ip = client_ip(
        headers,
        remote_addr,
        trust_proxy_headers,
        trusted_proxy_cidrs,
    );
    let now = Instant::now();
    let window = Duration::from_secs(60);
    let mut hits = state.tunnel_create_hits.write().await;
    let bucket = hits.entry(ip).or_default();
    while bucket
        .front()
        .is_some_and(|seen| now.duration_since(*seen) > window)
    {
        bucket.pop_front();
    }
    if bucket.len() >= limit_per_ip as usize {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many tunnel creation requests",
        ));
    }
    bucket.push_back(now);
    Ok(())
}

async fn require_tunnel_token(
    pool: &SqlitePool,
    id: &str,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<()> {
    let row = sqlx::query("SELECT token_hash FROM tunnels WHERE id = ?1 AND status != 'deleted'")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| {
            AppError::new(
                StatusCode::NOT_FOUND,
                "tunnel_not_found",
                "tunnel not found",
            )
        })?;
    let token_hash: String = row.get("token_hash");
    let token = bearer_token(headers).or(query_token);
    if token.is_some_and(|token| verify_token(token, &token_hash)) {
        Ok(())
    } else {
        Err(AppError::unauthorized())
    }
}

fn push_limit_offset(
    builder: &mut sqlx::QueryBuilder<'_, sqlx::Sqlite>,
    order_by: &str,
    limit: Option<i64>,
    offset: Option<i64>,
    default_limit: i64,
    max_limit: i64,
) {
    let (limit, offset) = page_bounds_with_max(limit, offset, default_limit, max_limit);
    builder.push(" ORDER BY ");
    builder.push(order_by);
    builder.push(" DESC LIMIT ");
    builder.push_bind(limit);
    builder.push(" OFFSET ");
    builder.push_bind(offset);
}

fn page_bounds(limit: Option<i64>, offset: Option<i64>, default_limit: i64) -> (i64, i64) {
    page_bounds_with_max(limit, offset, default_limit, MAX_PAGE_LIMIT)
}

fn page_bounds_with_max(
    limit: Option<i64>,
    offset: Option<i64>,
    default_limit: i64,
    max_limit: i64,
) -> (i64, i64) {
    (
        limit.unwrap_or(default_limit).clamp(1, max_limit),
        offset.unwrap_or(0).max(0),
    )
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn validate_access_policy(value: &str) -> Result<&'static str> {
    match value.trim() {
        "public" => Ok("public"),
        "bearer" => Ok("bearer"),
        "basic" => Ok("basic"),
        _ => Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_access_policy",
            "access_policy must be one of public, bearer, or basic",
        )),
    }
}

fn normalize_methods(methods: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for method in methods {
        let method = method.trim().to_ascii_uppercase();
        if method.is_empty() {
            continue;
        }
        if !method
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_allowed_method",
                "allowed_methods entries must be HTTP method tokens",
            ));
        }
        normalized.push(method);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_path_prefixes(prefixes: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for prefix in prefixes {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            continue;
        }
        if !prefix.starts_with('/') {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_blocked_path_prefix",
                "blocked_path_prefixes entries must start with /",
            ));
        }
        normalized.push(prefix.to_string());
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn optional_json_array(values: &[String]) -> Option<String> {
    if values.is_empty() {
        None
    } else {
        serde_json::to_string(values).ok()
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn verify_turnstile_if_required(
    cfg: &http_tunnel_common::ServerConfig,
    token: Option<&str>,
) -> Result<()> {
    let Some(secret) = cfg
        .turnstile_secret
        .as_deref()
        .filter(|secret| !secret.is_empty())
    else {
        return Ok(());
    };
    let Some(token) = token.and_then(|value| non_empty(Some(value))) else {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "turnstile_required",
            "turnstile verification is required",
        ));
    };
    let response = reqwest::Client::new()
        .post(&cfg.turnstile_verify_url)
        .form(&[("secret", secret), ("response", token)])
        .send()
        .await
        .map_err(AppError::internal)?
        .json::<serde_json::Value>()
        .await
        .map_err(AppError::internal)?;
    if response["success"].as_bool().unwrap_or(false) {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "turnstile_failed",
            "turnstile verification failed",
        ))
    }
}

async fn expire_inactive_tunnels(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "UPDATE tunnels SET status = 'expired' \
         WHERE status IN ('reserved', 'disconnected') AND expires_at <= CURRENT_TIMESTAMP",
    )
    .execute(pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_country_header_wins_over_other_sources() {
        let cfg = http_tunnel_common::ServerConfig::default();
        let source = ClientSource {
            ip: "198.51.100.10".to_string(),
            header_country_code: Some("US".to_string()),
            remote_country: Some(geoip::CountryLocation {
                country_code: "DE".to_string(),
                country: Some("Germany".to_string()),
            }),
        };
        let report = ClientSourceReport {
            public_ip: "127.0.0.1".to_string(),
            country_code: None,
            country: None,
            checked_at_unix_seconds: Some(1),
        };

        let resolved = resolve_client_source(&cfg, &source, Some(&report));

        assert_eq!(resolved.country_code.as_deref(), Some("US"));
        assert_eq!(resolved.country_source, Some("cf_header"));
        assert_eq!(resolved.reported_ip, None);
    }

    #[test]
    fn remote_geoip_is_fallback_when_report_is_not_public() {
        let cfg = http_tunnel_common::ServerConfig::default();
        let source = ClientSource {
            ip: "198.51.100.10".to_string(),
            header_country_code: None,
            remote_country: Some(geoip::CountryLocation {
                country_code: "JP".to_string(),
                country: Some("Japan".to_string()),
            }),
        };
        let report = ClientSourceReport {
            public_ip: "10.0.0.1".to_string(),
            country_code: None,
            country: None,
            checked_at_unix_seconds: Some(1),
        };

        let resolved = resolve_client_source(&cfg, &source, Some(&report));

        assert_eq!(resolved.country_code.as_deref(), Some("JP"));
        assert_eq!(resolved.country.as_deref(), Some("Japan"));
        assert_eq!(resolved.country_source, Some("remote_geoip"));
        assert_eq!(resolved.reported_ip, None);
    }

    #[test]
    fn public_reported_ip_is_stored_even_without_geoip_match() {
        let cfg = http_tunnel_common::ServerConfig::default();
        let source = ClientSource {
            ip: "198.51.100.10".to_string(),
            header_country_code: None,
            remote_country: None,
        };
        let report = ClientSourceReport {
            public_ip: "8.8.8.8".to_string(),
            country_code: None,
            country: None,
            checked_at_unix_seconds: Some(1),
        };

        let resolved = resolve_client_source(&cfg, &source, Some(&report));

        assert_eq!(resolved.country_code, None);
        assert_eq!(resolved.country_source, None);
        assert_eq!(resolved.reported_ip.as_deref(), Some("8.8.8.8"));
    }

    #[test]
    fn client_reported_country_is_fallback_without_geoip_match() {
        let cfg = http_tunnel_common::ServerConfig::default();
        let source = ClientSource {
            ip: "198.51.100.10".to_string(),
            header_country_code: None,
            remote_country: None,
        };
        let report = ClientSourceReport {
            public_ip: "8.8.8.8".to_string(),
            country_code: Some("US".to_string()),
            country: Some("United States".to_string()),
            checked_at_unix_seconds: Some(1),
        };

        let resolved = resolve_client_source(&cfg, &source, Some(&report));

        assert_eq!(resolved.country_code.as_deref(), Some("US"));
        assert_eq!(resolved.country.as_deref(), Some("United States"));
        assert_eq!(resolved.country_source, Some("client_report"));
        assert_eq!(resolved.reported_ip.as_deref(), Some("8.8.8.8"));
    }
}
