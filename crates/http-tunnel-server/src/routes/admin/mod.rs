use crate::{
    error::{AppError, Result},
    net::client_ip,
    state::AppState,
};
use axum::{
    extract::{connect_info::ConnectInfo, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use http_tunnel_common::{api::ApiResponse, ids::generate_event_id, token::hash_token};
use serde::Serialize;
use sqlx::Row;
use std::{net::SocketAddr, time::UNIX_EPOCH};

mod alerts;
mod auth;
mod backup;
mod config;
mod logs;
mod maintenance;
mod upgrade;

pub use alerts::alerts;
use auth::bearer_token_valid;
use auth::verify_session_cookie;
pub use auth::{
    change_password, login, logout, require_admin, require_admin_write, revoke_all_sessions,
    revoke_session, sessions,
};
pub use backup::{backup, restore_validate};
pub(crate) use config::validate_server_config;
pub use config::{
    clear_metrics_token, clear_tunnel_create_token, clear_turnstile_secret, config_schema,
    get_config, put_config, reload_config, rotate_metrics_token, rotate_tunnel_create_token,
    set_turnstile_secret, validate_config,
};
mod diagnostics;
pub use diagnostics::{diagnostics, diagnostics_export};
pub use logs::{
    audit_export, audit_logs, logs, recent_events, recent_requests, request_detail, request_replay,
    requests_export,
};
pub use maintenance::{analyze, maintenance_status, vacuum, wal_checkpoint};
pub use upgrade::{restart, upgrade, upgrade_ws, validate_upgrade};

pub(crate) struct AuditLog<'a> {
    pub actor_token: Option<&'a str>,
    pub action: &'a str,
    pub target_type: Option<&'a str>,
    pub target_id: Option<&'a str>,
    pub result: &'a str,
    pub detail: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct AdminStatus {
    pub setup_required: bool,
    pub pending_restart: bool,
    pub active_sessions: usize,
    pub request_count: i64,
    pub error_count: i64,
    pub uptime_seconds: u64,
}

pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<AdminStatus>>> {
    require_admin(&state, &headers).await?;
    Ok(Json(ApiResponse::ok(build_admin_status(&state).await?)))
}

pub(crate) async fn build_admin_status(state: &AppState) -> Result<AdminStatus> {
    let cfg = state.config.read().await;
    let pending_restart = get_pending_restart(state).await.unwrap_or(false);
    let active_sessions = state.active_session_count().await;
    let request_count = sqlx::query("SELECT COUNT(*) AS count FROM request_logs")
        .fetch_one(&state.pool)
        .await
        .map(|row| row.get::<i64, _>("count"))
        .unwrap_or_default();
    let error_count =
        sqlx::query("SELECT COUNT(*) AS count FROM request_logs WHERE error IS NOT NULL")
            .fetch_one(&state.pool)
            .await
            .map(|row| row.get::<i64, _>("count"))
            .unwrap_or_default();
    let uptime_seconds = state
        .started_at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|started| {
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|now| now.saturating_sub(started).as_secs())
        })
        .unwrap_or_default();
    Ok(AdminStatus {
        setup_required: cfg.setup_required(),
        pending_restart,
        active_sessions,
        request_count,
        error_count,
        uptime_seconds,
    })
}

pub async fn cleanup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<crate::app::CleanupSummary>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let summary = crate::app::cleanup_once(&state)
        .await
        .map_err(AppError::internal)?;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "cleanup",
            target_type: Some("system"),
            target_id: None,
            result: "success",
            detail: Some("manual cleanup"),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(summary)))
}

async fn add_audit_event(state: &AppState, kind: &str, message: Option<&str>) -> Result<()> {
    sqlx::query("INSERT INTO events (id, kind, message) VALUES (?1, ?2, ?3)")
        .bind(generate_event_id())
        .bind(kind)
        .bind(message)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}

pub(crate) async fn record_admin_audit(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    entry: AuditLog<'_>,
) -> Result<()> {
    let cfg = state.config.read().await;
    let remote_ip = client_ip(
        headers,
        remote_addr,
        cfg.trust_proxy_headers,
        &cfg.trusted_proxy_cidrs,
    );
    drop(cfg);
    record_admin_audit_with_ip(state, Some(remote_ip), entry).await
}

pub(crate) async fn record_admin_audit_without_remote(
    state: &AppState,
    entry: AuditLog<'_>,
) -> Result<()> {
    record_admin_audit_with_ip(state, None, entry).await
}

async fn record_admin_audit_with_ip(
    state: &AppState,
    remote_ip: Option<String>,
    entry: AuditLog<'_>,
) -> Result<()> {
    let actor = entry.actor_token.map(actor_fingerprint);
    let detail = entry.detail.map(crate::redaction::redact_text);
    sqlx::query(
        "INSERT INTO audit_logs (id, actor, remote_ip, action, target_type, target_id, result, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(generate_event_id())
    .bind(actor)
    .bind(remote_ip)
    .bind(entry.action)
    .bind(entry.target_type)
    .bind(entry.target_id)
    .bind(entry.result)
    .bind(detail.as_deref())
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

pub(crate) fn list_response<T: Serialize>(
    data: T,
    total_count: i64,
    limit: i64,
    offset: i64,
) -> Result<Response> {
    let mut response = Json(ApiResponse::ok(data)).into_response();
    let has_more = offset.saturating_add(limit) < total_count;
    for (name, value) in [
        ("x-http-tunnel-total-count", total_count.to_string()),
        ("x-http-tunnel-limit", limit.to_string()),
        ("x-http-tunnel-offset", offset.to_string()),
        ("x-http-tunnel-has-more", has_more.to_string()),
    ] {
        response.headers_mut().insert(
            name,
            HeaderValue::from_str(&value).map_err(AppError::internal)?,
        );
    }
    Ok(response)
}

fn actor_fingerprint(token: &str) -> String {
    let hash = hash_token(token);
    format!("admin:{}", &hash[..12.min(hash.len())])
}

async fn get_pending_restart(state: &AppState) -> Result<bool> {
    let value = sqlx::query("SELECT value FROM settings WHERE key = 'pending_restart'")
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?
        .map(|row| row.get::<String, _>("value"))
        .unwrap_or_else(|| "false".to_string());
    Ok(value == "true")
}

async fn set_pending_restart(state: &AppState, value: bool) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, category, requires_restart) VALUES ('pending_restart', ?1, 'runtime', FALSE) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(if value { "true" } else { "false" })
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}
