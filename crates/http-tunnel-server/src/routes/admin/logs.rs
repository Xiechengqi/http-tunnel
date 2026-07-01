use super::*;
use crate::routes::tunnels::{
    count_event_rows, count_request_rows, event_rows, request_row_to_json, request_rows,
    EventListQuery, RequestListQuery,
};
use axum::{
    extract::{connect_info::ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use http_tunnel_common::api::ApiResponse;
use http_tunnel_common::ids::generate_request_id;
use http_tunnel_protocol::{
    types::{decode_payload, encode_payload, ErrorPayload, RequestStart, ResponseStart},
    Frame, FrameType,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::{
    borrow::Cow,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const DEFAULT_LOG_LIMIT: i64 = 100;
const MAX_PAGE_LIMIT: i64 = 500;
const EXPORT_ROW_LIMIT: i64 = 10_000;

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct LogListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub kind: Option<String>,
    pub error_only: Option<bool>,
    pub q: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct AuditListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub all: Option<bool>,
    pub action: Option<String>,
    pub result: Option<String>,
    pub target_type: Option<String>,
    pub q: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReplayResponse {
    pub request_id: String,
    pub replay_of: String,
    pub status: u16,
    pub headers: serde_json::Value,
    pub body_preview: String,
    pub body_preview_encoding: String,
    pub body_truncated: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct ReplayRequest {
    pub method: Option<String>,
    pub path: Option<String>,
    pub headers: Option<Vec<ReplayHeader>>,
    pub body: Option<String>,
    pub body_encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReplayHeader {
    pub name: String,
    pub value: String,
}

pub async fn recent_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RequestListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_request_rows(&state.pool, None, &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_LOG_LIMIT);
    let rows = request_rows(&state.pool, None, query).await?;
    list_response(rows, total_count, limit, offset)
}

pub async fn requests_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RequestListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_request_rows(&state.pool, None, &query).await?;
    let export_all = query.all.unwrap_or(false);
    let export_query = export_request_query(query);
    let rows = request_rows(&state.pool, None, export_query).await?;
    let row_count = rows.len();
    let truncated = export_all && total_count > EXPORT_ROW_LIMIT;
    let mut csv = String::from(
        "id,type,tunnel_id,session_id,method,path,host,remote_ip,status,started_at,completed_at,duration_ms,bytes_in,bytes_out,error,replay_of\n",
    );
    for row in rows {
        push_csv_row(
            &mut csv,
            &[
                json_string(&row, "id"),
                json_string(&row, "type"),
                json_string(&row, "tunnel_id"),
                json_string(&row, "session_id"),
                json_string(&row, "method"),
                json_string(&row, "path"),
                json_string(&row, "host"),
                json_string(&row, "remote_ip"),
                json_string(&row, "status"),
                json_string(&row, "started_at"),
                json_string(&row, "completed_at"),
                json_string(&row, "duration_ms"),
                json_string(&row, "bytes_in"),
                json_string(&row, "bytes_out"),
                json_string(&row, "error"),
                json_string(&row, "replay_of"),
            ],
        );
    }
    Ok(csv_response(
        "http-tunnel-requests.csv",
        csv,
        row_count,
        total_count,
        truncated,
    ))
}

pub async fn request_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>> {
    require_admin(&state, &headers).await?;
    let row = sqlx::query(
        "SELECT r.id, r.tunnel_id, r.session_id, r.request_type, r.method, r.path, r.host, r.remote_ip, r.user_agent, \
                r.status, r.started_at, r.completed_at, r.duration_ms, r.bytes_in, r.bytes_out, r.error, \
                r.ws_message_count, r.ws_close_code, r.ws_close_reason, r.replay_of, \
                t.subdomain AS tunnel_subdomain, t.status AS tunnel_status, \
                s.disconnect_reason AS session_disconnect_reason, s.client_version AS session_client_version \
         FROM request_logs r \
         LEFT JOIN tunnels t ON t.id = r.tunnel_id \
         LEFT JOIN sessions s ON s.id = r.session_id \
         WHERE r.id = ?1",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            "request_not_found",
            "request log not found",
        )
    })?;
    let tunnel_subdomain = row
        .try_get::<Option<String>, _>("tunnel_subdomain")
        .ok()
        .flatten();
    let tunnel_status = row
        .try_get::<Option<String>, _>("tunnel_status")
        .ok()
        .flatten();
    let session_disconnect_reason = row
        .try_get::<Option<String>, _>("session_disconnect_reason")
        .ok()
        .flatten();
    let session_client_version = row
        .try_get::<Option<String>, _>("session_client_version")
        .ok()
        .flatten();
    let inspection = load_request_inspection(&state, &id).await?;
    let mut value = request_row_to_json(row);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "tunnel".to_string(),
            serde_json::json!({
                "id": object.get("tunnel_id").cloned().unwrap_or(serde_json::Value::Null),
                "subdomain": tunnel_subdomain,
                "status": tunnel_status,
            }),
        );
        object.insert(
            "session".to_string(),
            serde_json::json!({
                "id": object.get("session_id").cloned().unwrap_or(serde_json::Value::Null),
                "disconnect_reason": session_disconnect_reason,
                "client_version": session_client_version,
            }),
        );
        object.insert("inspection".to_string(), inspection);
    }
    Ok(Json(ApiResponse::ok(value)))
}

pub async fn request_replay(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    payload: Option<Json<ReplayRequest>>,
) -> Result<Json<ApiResponse<ReplayResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let payload = payload.map(|Json(value)| value).unwrap_or_default();
    let row = sqlx::query(
        "SELECT r.tunnel_id, r.request_type, r.method, r.path, t.subdomain \
         FROM request_logs r \
         JOIN tunnels t ON t.id = r.tunnel_id \
         WHERE r.id = ?1 AND t.status != 'deleted'",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            "request_not_found",
            "request log not found",
        )
    })?;
    if row.get::<String, _>("request_type") != "http" {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "request_not_replayable",
            "only HTTP requests can be replayed",
        ));
    }
    let inspection = sqlx::query(
        "SELECT request_headers, request_body_preview, request_body_truncated \
         FROM request_inspections WHERE request_log_id = ?1",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::CONFLICT,
            "inspection_missing",
            "request inspection data is not available",
        )
    })?;
    if payload.body.is_none()
        && inspection
            .try_get::<bool, _>("request_body_truncated")
            .unwrap_or(false)
    {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "request_body_truncated",
            "truncated request bodies cannot be replayed safely",
        ));
    }
    let tunnel_id = row.get::<String, _>("tunnel_id");
    let subdomain = row.get::<String, _>("subdomain");
    let cfg = state.config.read().await.clone();
    let Some(session) = state
        .select_session(
            &subdomain,
            &tunnel_id,
            crate::state::effective_session_pool_policy(&cfg),
        )
        .await
    else {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "tunnel_offline",
            "tunnel is offline",
        ));
    };
    let replay_method = replay_method(payload.method.as_deref(), &row.get::<String, _>("method"))?;
    let replay_path = replay_path(payload.path.as_deref(), &row.get::<String, _>("path"))?;
    let replay_headers = match payload.headers {
        Some(headers) => replay_headers(headers)?,
        None => parse_header_array(&inspection.get::<String, _>("request_headers")),
    };
    let replay_body = match payload.body.as_deref() {
        Some(body) => replay_body(body, payload.body_encoding.as_deref())?,
        None => inspection
            .try_get::<Vec<u8>, _>("request_body_preview")
            .unwrap_or_default(),
    };
    let replayed = replay_request(
        &state,
        &session,
        &tunnel_id,
        &id,
        replay_method,
        replay_path,
        replay_headers,
        replay_body,
        cfg.request_timeout_seconds.max(1),
        cfg.inspector_max_body_preview_bytes,
    )
    .await?;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "request_replay",
            target_type: Some("request"),
            target_id: Some(&id),
            result: "success",
            detail: Some(&tunnel_id),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(replayed)))
}

pub async fn recent_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_event_rows(&state.pool, None, &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_LOG_LIMIT);
    let rows = event_rows(&state.pool, None, query).await?;
    list_response(rows, total_count, limit, offset)
}

pub async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LogListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_log_rows(&state.pool, &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_LOG_LIMIT);
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT source, kind, detail, created_at FROM ( \
         SELECT 'event' AS source, kind, COALESCE(message, '') AS detail, created_at, NULL AS error \
         FROM events \
         UNION ALL \
         SELECT 'request' AS source, \
                COALESCE(method, '') || ' ' || COALESCE(path, '') AS kind, \
                COALESCE(error, 'status=' || COALESCE(status, 'pending')) AS detail, \
                started_at AS created_at, error \
         FROM request_logs",
    );
    builder.push(") WHERE 1 = 1");
    if let Some(kind) = non_empty(query.kind.as_deref()) {
        builder.push(" AND kind = ").push_bind(kind.to_string());
    }
    if query.error_only.unwrap_or(false) {
        builder.push(" AND source = 'request' AND error IS NOT NULL");
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (kind LIKE ")
            .push_bind(pattern.clone())
            .push(" OR detail LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    builder.push(" ORDER BY created_at DESC LIMIT ");
    builder.push_bind(
        query
            .limit
            .unwrap_or(DEFAULT_LOG_LIMIT)
            .clamp(1, MAX_PAGE_LIMIT),
    );
    builder.push(" OFFSET ");
    builder.push_bind(query.offset.unwrap_or(0).max(0));
    let rows = builder
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::internal)?;
    let values: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "source": row.get::<String, _>("source"),
                "kind": row.get::<String, _>("kind"),
                "detail": row.get::<String, _>("detail"),
                "created_at": row.get::<String, _>("created_at"),
            })
        })
        .collect();
    list_response(values, total_count, limit, offset)
}

async fn count_log_rows(pool: &SqlitePool, query: &LogListQuery) -> Result<i64> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT COUNT(*) AS count FROM ( \
         SELECT 'event' AS source, kind, COALESCE(message, '') AS detail, created_at, NULL AS error \
         FROM events \
         UNION ALL \
         SELECT 'request' AS source, \
                COALESCE(method, '') || ' ' || COALESCE(path, '') AS kind, \
                COALESCE(error, 'status=' || COALESCE(status, 'pending')) AS detail, \
                started_at AS created_at, error \
         FROM request_logs",
    );
    builder.push(") WHERE 1 = 1");
    if let Some(kind) = non_empty(query.kind.as_deref()) {
        builder.push(" AND kind = ").push_bind(kind.to_string());
    }
    if query.error_only.unwrap_or(false) {
        builder.push(" AND source = 'request' AND error IS NOT NULL");
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (kind LIKE ")
            .push_bind(pattern.clone())
            .push(" OR detail LIKE ")
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

pub async fn audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuditListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_audit_log_rows(&state.pool, &query).await?;
    let (limit, offset) = page_bounds(query.limit, query.offset, DEFAULT_LOG_LIMIT);
    let rows = audit_log_rows(&state.pool, query).await?;
    list_response(rows, total_count, limit, offset)
}

pub async fn audit_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuditListQuery>,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let total_count = count_audit_log_rows(&state.pool, &query).await?;
    let export_all = query.all.unwrap_or(false);
    let export_query = export_audit_query(query);
    let rows = audit_log_rows(&state.pool, export_query).await?;
    let row_count = rows.len();
    let truncated = export_all && total_count > EXPORT_ROW_LIMIT;
    let mut csv =
        String::from("actor,remote_ip,action,target_type,target_id,result,detail,created_at\n");
    for row in rows {
        push_csv_row(
            &mut csv,
            &[
                json_string(&row, "actor"),
                json_string(&row, "remote_ip"),
                json_string(&row, "action"),
                json_string(&row, "target_type"),
                json_string(&row, "target_id"),
                json_string(&row, "result"),
                json_string(&row, "detail"),
                json_string(&row, "created_at"),
            ],
        );
    }
    Ok(csv_response(
        "http-tunnel-audit.csv",
        csv,
        row_count,
        total_count,
        truncated,
    ))
}

async fn count_audit_log_rows(pool: &SqlitePool, query: &AuditListQuery) -> Result<i64> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT COUNT(*) AS count FROM audit_logs WHERE 1 = 1",
    );
    if let Some(action) = non_empty(query.action.as_deref()) {
        builder.push(" AND action = ").push_bind(action.to_string());
    }
    if let Some(result) = non_empty(query.result.as_deref()) {
        builder.push(" AND result = ").push_bind(result.to_string());
    }
    if let Some(target_type) = non_empty(query.target_type.as_deref()) {
        builder
            .push(" AND target_type = ")
            .push_bind(target_type.to_string());
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (actor LIKE ")
            .push_bind(pattern.clone())
            .push(" OR remote_ip LIKE ")
            .push_bind(pattern.clone())
            .push(" OR action LIKE ")
            .push_bind(pattern.clone())
            .push(" OR target_id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR detail LIKE ")
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

async fn audit_log_rows(
    pool: &SqlitePool,
    query: AuditListQuery,
) -> Result<Vec<serde_json::Value>> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT actor, remote_ip, action, target_type, target_id, result, detail, created_at \
         FROM audit_logs WHERE 1 = 1",
    );
    if let Some(action) = non_empty(query.action.as_deref()) {
        builder.push(" AND action = ").push_bind(action.to_string());
    }
    if let Some(result) = non_empty(query.result.as_deref()) {
        builder.push(" AND result = ").push_bind(result.to_string());
    }
    if let Some(target_type) = non_empty(query.target_type.as_deref()) {
        builder
            .push(" AND target_type = ")
            .push_bind(target_type.to_string());
    }
    if let Some(q) = non_empty(query.q.as_deref()) {
        let pattern = format!("%{q}%");
        builder
            .push(" AND (actor LIKE ")
            .push_bind(pattern.clone())
            .push(" OR remote_ip LIKE ")
            .push_bind(pattern.clone())
            .push(" OR action LIKE ")
            .push_bind(pattern.clone())
            .push(" OR target_id LIKE ")
            .push_bind(pattern.clone())
            .push(" OR detail LIKE ")
            .push_bind(pattern)
            .push(")");
    }
    builder.push(" ORDER BY created_at DESC LIMIT ");
    let max_limit = if query.all.unwrap_or(false) {
        EXPORT_ROW_LIMIT
    } else {
        MAX_PAGE_LIMIT
    };
    builder.push_bind(query.limit.unwrap_or(DEFAULT_LOG_LIMIT).clamp(1, max_limit));
    builder.push(" OFFSET ");
    builder.push_bind(query.offset.unwrap_or(0).max(0));
    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "actor": row.try_get::<Option<String>, _>("actor").ok().flatten(),
                "remote_ip": row.try_get::<Option<String>, _>("remote_ip").ok().flatten(),
                "action": row.get::<String, _>("action"),
                "target_type": row.try_get::<Option<String>, _>("target_type").ok().flatten(),
                "target_id": row.try_get::<Option<String>, _>("target_id").ok().flatten(),
                "result": row.get::<String, _>("result"),
                "detail": row.try_get::<Option<String>, _>("detail").ok().flatten(),
                "created_at": row.get::<String, _>("created_at"),
            })
        })
        .collect())
}

async fn load_request_inspection(state: &AppState, id: &str) -> Result<serde_json::Value> {
    let row = sqlx::query(
        "SELECT request_headers, request_content_type, request_body_preview, request_body_preview_encoding, request_body_truncated, response_status, response_headers, \
                response_content_type, response_body_preview, response_body_preview_encoding, response_body_truncated, created_at, updated_at \
         FROM request_inspections WHERE request_log_id = ?1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?;
    let Some(row) = row else {
        return Ok(serde_json::Value::Null);
    };
    let request_body = row
        .try_get::<Vec<u8>, _>("request_body_preview")
        .unwrap_or_default();
    let response_body = row
        .try_get::<Vec<u8>, _>("response_body_preview")
        .unwrap_or_default();
    let request_encoding = row
        .try_get::<Option<String>, _>("request_body_preview_encoding")
        .ok()
        .flatten()
        .unwrap_or_else(|| "utf8".to_string());
    let response_encoding = row
        .try_get::<Option<String>, _>("response_body_preview_encoding")
        .ok()
        .flatten()
        .unwrap_or_else(|| "utf8".to_string());
    Ok(serde_json::json!({
        "request_headers": parse_header_value(row.try_get::<String, _>("request_headers").ok().as_deref()),
        "request_content_type": row.try_get::<Option<String>, _>("request_content_type").ok().flatten(),
        "request_body_preview": preview_value(&request_body, &request_encoding),
        "request_body_preview_encoding": request_encoding,
        "request_body_truncated": row.try_get::<bool, _>("request_body_truncated").unwrap_or(false),
        "response_status": row.try_get::<Option<i64>, _>("response_status").ok().flatten(),
        "response_headers": parse_header_value(row.try_get::<Option<String>, _>("response_headers").ok().flatten().as_deref()),
        "response_content_type": row.try_get::<Option<String>, _>("response_content_type").ok().flatten(),
        "response_body_preview": preview_value(&response_body, &response_encoding),
        "response_body_preview_encoding": response_encoding,
        "response_body_truncated": row.try_get::<bool, _>("response_body_truncated").unwrap_or(false),
        "created_at": row.get::<String, _>("created_at"),
        "updated_at": row.get::<String, _>("updated_at"),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn replay_request(
    state: &AppState,
    session: &crate::state::ActiveSession,
    tunnel_id: &str,
    replay_of: &str,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout_seconds: u64,
    preview_limit: usize,
) -> Result<ReplayResponse> {
    let stream_id = state.next_stream_id();
    let request_log_id = generate_request_id();
    let started_at = Instant::now();
    let (response_tx, mut response_rx) = mpsc::channel::<Frame>(64);
    state
        .insert_pending_stream(
            stream_id,
            crate::state::PendingStream {
                tunnel_id: tunnel_id.to_string(),
                session_id: session.session_id.clone(),
                stream_type: crate::state::PendingStreamType::Http,
                tx: response_tx,
                session_metrics: session.metrics.clone(),
            },
        )
        .await;
    let _ = sqlx::query(
        "INSERT INTO request_logs (id, tunnel_id, session_id, request_type, method, path, bytes_in, replay_of) \
         VALUES (?1, ?2, ?3, 'http_replay', ?4, ?5, ?6, ?7)",
    )
    .bind(&request_log_id)
    .bind(tunnel_id)
    .bind(&session.session_id)
    .bind(&method)
    .bind(&path)
    .bind(i64::try_from(body.len()).unwrap_or(i64::MAX))
    .bind(replay_of)
    .execute(&state.pool)
    .await;

    let result = tokio::time::timeout(Duration::from_secs(timeout_seconds), async {
        crate::routes::proxy::send_frame(
            session,
            Frame::new(
                FrameType::RequestStart,
                stream_id,
                encode_payload(&RequestStart {
                    method: method.clone(),
                    path: path.clone(),
                    headers,
                })
                .map_err(|error| error.to_string())?,
            ),
        )
        .await?;
        if !body.is_empty() {
            session.metrics.add_bytes_in(body.len());
            crate::routes::proxy::send_frame(
                session,
                Frame::new(FrameType::RequestBody, stream_id, body),
            )
            .await?;
        }
        crate::routes::proxy::send_frame(
            session,
            Frame::new(FrameType::RequestEnd, stream_id, Vec::new()),
        )
        .await?;
        let start = loop {
            let Some(frame) = response_rx.recv().await else {
                return Err("tunnel disconnected before response start".to_string());
            };
            match frame.frame_type {
                FrameType::ResponseStart => {
                    break decode_payload::<ResponseStart>(&frame.payload)
                        .map_err(|error| error.to_string())?;
                }
                FrameType::Error => {
                    let payload = decode_payload::<ErrorPayload>(&frame.payload)
                        .map_err(|error| error.to_string())?;
                    return Err(format!("{}: {}", payload.code, payload.message));
                }
                _ => {}
            }
        };
        let mut body_preview = Vec::new();
        let mut body_truncated = false;
        while let Some(frame) = response_rx.recv().await {
            match frame.frame_type {
                FrameType::ResponseBody => {
                    session.metrics.add_bytes_out(frame.payload.len());
                    push_preview(
                        &frame.payload,
                        preview_limit,
                        &mut body_preview,
                        &mut body_truncated,
                    );
                }
                FrameType::ResponseEnd => break,
                FrameType::Error | FrameType::Cancel => break,
                _ => {}
            }
        }
        Ok::<_, String>((start, body_preview, body_truncated))
    })
    .await;
    state.remove_pending_stream(stream_id).await;

    match result {
        Ok(Ok((start, body_preview, body_truncated))) => {
            let content_type = header_value(&start.headers, "content-type");
            let encoding = preview_encoding(content_type.as_deref(), &body_preview);
            update_replay_log(
                state,
                &request_log_id,
                start.status,
                i64::try_from(body_preview.len()).unwrap_or(i64::MAX),
                None,
                started_at,
            )
            .await;
            Ok(ReplayResponse {
                request_id: request_log_id,
                replay_of: replay_of.to_string(),
                status: start.status,
                headers: headers_to_value(&start.headers),
                body_preview: preview_value(&body_preview, encoding),
                body_preview_encoding: encoding.to_string(),
                body_truncated,
            })
        }
        Ok(Err(error)) => {
            update_replay_log(
                state,
                &request_log_id,
                StatusCode::BAD_GATEWAY.as_u16(),
                0,
                Some(&error),
                started_at,
            )
            .await;
            Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                "replay_failed",
                error,
            ))
        }
        Err(_) => {
            let _ = crate::routes::proxy::send_frame(
                session,
                Frame::new(FrameType::Cancel, stream_id, b"replay_timeout".to_vec()),
            )
            .await;
            update_replay_log(
                state,
                &request_log_id,
                StatusCode::GATEWAY_TIMEOUT.as_u16(),
                0,
                Some("replay_timeout"),
                started_at,
            )
            .await;
            Err(AppError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "replay_timeout",
                "replay timed out",
            ))
        }
    }
}

async fn update_replay_log(
    state: &AppState,
    request_log_id: &str,
    status: u16,
    bytes_out: i64,
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

fn parse_header_array(raw: &str) -> Vec<(String, String)> {
    serde_json::from_str::<Vec<serde_json::Value>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let name = value.get("name")?.as_str()?.to_string();
            let header_value = value.get("value")?.as_str()?.to_string();
            if header_value == "[redacted]" {
                None
            } else {
                Some((name, header_value))
            }
        })
        .collect()
}

fn replay_method(override_value: Option<&str>, original: &str) -> Result<String> {
    let method = override_value
        .unwrap_or(original)
        .trim()
        .to_ascii_uppercase();
    if method.is_empty()
        || !method
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_replay_method",
            "replay method must be a valid HTTP method token",
        ));
    }
    Ok(method)
}

fn replay_path(override_value: Option<&str>, original: &str) -> Result<String> {
    let path = override_value.unwrap_or(original).trim();
    if !path.starts_with('/') || path.contains("://") {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_replay_path",
            "replay path must be an origin-form path",
        ));
    }
    Ok(path.to_string())
}

fn replay_headers(headers: Vec<ReplayHeader>) -> Result<Vec<(String, String)>> {
    let mut values = Vec::new();
    for header in headers {
        let name = HeaderName::from_bytes(header.name.trim().as_bytes()).map_err(|_| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_replay_header",
                "replay header names must be valid HTTP header names",
            )
        })?;
        if http_tunnel_common::headers::is_hop_by_hop(&name)
            || !crate::routes::proxy::forwarded_header_allowed(&name)
        {
            continue;
        }
        let value = HeaderValue::from_str(&header.value).map_err(|_| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_replay_header",
                "replay header values must be valid HTTP header values",
            )
        })?;
        let value = value.to_str().map_err(|_| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_replay_header",
                "replay header values must be UTF-8",
            )
        })?;
        values.push((name.as_str().to_string(), value.to_string()));
    }
    Ok(values)
}

fn replay_body(body: &str, encoding: Option<&str>) -> Result<Vec<u8>> {
    match encoding
        .unwrap_or("utf8")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "utf8" | "text" => Ok(body.as_bytes().to_vec()),
        "base64" => BASE64_STANDARD.decode(body).map_err(|_| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_replay_body",
                "base64 replay body could not be decoded",
            )
        }),
        _ => Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_replay_body_encoding",
            "body_encoding must be utf8 or base64",
        )),
    }
}

fn parse_header_value(raw: Option<&str>) -> serde_json::Value {
    raw.and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()))
}

fn preview_value(bytes: &[u8], encoding: &str) -> String {
    if encoding == "base64" {
        BASE64_STANDARD.encode(bytes)
    } else {
        String::from_utf8_lossy(bytes).to_string()
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

fn headers_to_value(headers: &[(String, String)]) -> serde_json::Value {
    headers
        .iter()
        .map(|(name, value)| {
            serde_json::json!({
                "name": name,
                "value": if sensitive_header(name) { "[redacted]" } else { value },
            })
        })
        .collect::<Vec<_>>()
        .into()
}

fn sensitive_header(name: &str) -> bool {
    crate::redaction::sensitive_key(name)
}

fn json_string(value: &serde_json::Value, key: &str) -> String {
    match value.get(key) {
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(serde_json::Value::Number(value)) => value.to_string(),
        Some(serde_json::Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn push_csv_row(csv: &mut String, fields: &[String]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            csv.push(',');
        }
        push_csv_field(csv, field);
    }
    csv.push('\n');
}

fn push_csv_field(csv: &mut String, field: &str) {
    let field = csv_safe_field(field);
    if field.contains([',', '"', '\n', '\r', '\t']) {
        csv.push('"');
        for ch in field.chars() {
            if ch == '"' {
                csv.push('"');
            }
            csv.push(ch);
        }
        csv.push('"');
    } else {
        csv.push_str(&field);
    }
}

fn csv_safe_field(field: &str) -> Cow<'_, str> {
    if field.starts_with(['=', '+', '-', '@', '\t', '\r', '\n']) {
        Cow::Owned(format!("'{field}"))
    } else {
        Cow::Borrowed(field)
    }
}

fn export_request_query(mut query: RequestListQuery) -> RequestListQuery {
    if query.all.unwrap_or(false) {
        query.limit = Some(EXPORT_ROW_LIMIT);
        query.offset = Some(0);
    }
    query
}

fn export_audit_query(mut query: AuditListQuery) -> AuditListQuery {
    if query.all.unwrap_or(false) {
        query.limit = Some(EXPORT_ROW_LIMIT);
        query.offset = Some(0);
    }
    query
}

fn page_bounds(limit: Option<i64>, offset: Option<i64>, default_limit: i64) -> (i64, i64) {
    (
        limit.unwrap_or(default_limit).clamp(1, MAX_PAGE_LIMIT),
        offset.unwrap_or(0).max(0),
    )
}

fn csv_response(
    filename: &str,
    body: String,
    row_count: usize,
    total_count: i64,
    truncated: bool,
) -> Response {
    let mut response = (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response();
    for (name, value) in [
        ("x-http-tunnel-export-row-count", row_count.to_string()),
        ("x-http-tunnel-export-total-count", total_count.to_string()),
        ("x-http-tunnel-export-truncated", truncated.to_string()),
    ] {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::push_csv_field;

    #[test]
    fn csv_fields_escape_delimiters_quotes_and_formula_prefixes() {
        let mut csv = String::new();
        push_csv_field(&mut csv, "plain");
        assert_eq!(csv, "plain");

        csv.clear();
        push_csv_field(&mut csv, "a,b\"c");
        assert_eq!(csv, "\"a,b\"\"c\"");

        csv.clear();
        push_csv_field(&mut csv, "=SUM(1,2)");
        assert_eq!(csv, "\"'=SUM(1,2)\"");

        csv.clear();
        push_csv_field(&mut csv, "\t=SUM(1,2)");
        assert_eq!(csv, "\"'\t=SUM(1,2)\"");
    }
}
