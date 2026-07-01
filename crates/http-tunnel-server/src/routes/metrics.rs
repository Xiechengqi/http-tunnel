use crate::state::PendingStreamType;
use crate::{net::proxy_is_trusted, routes::admin::require_admin, state::AppState};
use axum::{
    extract::{connect_info::ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use http_tunnel_common::token::verify_token;
use sqlx::Row;
use std::net::SocketAddr;

pub async fn metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Response {
    if !metrics_authorized(&state, &headers, remote_addr).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match build_metrics(&state).await {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "failed to build metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn metrics_authorized(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
) -> bool {
    let cfg = state.config.read().await.clone();
    if cfg.metrics_public {
        return true;
    }
    if proxy_is_trusted(remote_addr.ip(), &cfg.trusted_proxy_cidrs) {
        return true;
    }
    if let Some(hash) = cfg.metrics_bearer_token_hash.as_deref() {
        if bearer_token(headers).is_some_and(|token| verify_token(&token, hash)) {
            return true;
        }
    }
    drop(cfg);
    require_admin(state, headers).await.is_ok()
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(ToString::to_string)
}

async fn build_metrics(state: &AppState) -> anyhow::Result<String> {
    let mut out = String::new();
    push_gauge(
        &mut out,
        "http_tunnel_active_sessions",
        state.active_session_count().await as i64,
    );
    let pending_streams = state.pending_streams.read().await;
    push_gauge(
        &mut out,
        "http_tunnel_active_streams",
        pending_streams.len() as i64,
    );
    let active_http_streams = pending_streams
        .values()
        .filter(|stream| stream.stream_type == PendingStreamType::Http)
        .count() as i64;
    let active_ws_streams = pending_streams
        .values()
        .filter(|stream| stream.stream_type == PendingStreamType::WebSocket)
        .count() as i64;
    drop(pending_streams);
    push_labeled_gauge(
        &mut out,
        "http_tunnel_active_streams_by_type",
        &[("type", "http")],
        active_http_streams,
    );
    push_labeled_gauge(
        &mut out,
        "http_tunnel_active_streams_by_type",
        &[("type", "ws")],
        active_ws_streams,
    );
    for row in sqlx::query("SELECT status, COUNT(*) AS count FROM tunnels GROUP BY status")
        .fetch_all(&state.pool)
        .await?
    {
        push_labeled_gauge(
            &mut out,
            "http_tunnel_tunnels",
            &[("status", &row.get::<String, _>("status"))],
            row.get::<i64, _>("count"),
        );
    }
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_requests_total",
        "SELECT COUNT(*) AS value FROM request_logs",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_request_errors_total",
        "SELECT COUNT(*) AS value FROM request_logs WHERE error IS NOT NULL",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_request_bytes_in_total",
        "SELECT COALESCE(SUM(bytes_in), 0) AS value FROM request_logs",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_request_bytes_out_total",
        "SELECT COALESCE(SUM(bytes_out), 0) AS value FROM request_logs",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_websocket_sessions_total",
        "SELECT COUNT(*) AS value FROM request_logs WHERE request_type = 'ws'",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_websocket_messages_total",
        "SELECT COALESCE(SUM(ws_message_count), 0) AS value FROM request_logs WHERE request_type = 'ws'",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_stale_sessions_total",
        "SELECT COUNT(*) AS value FROM sessions WHERE disconnect_reason = 'stale_session'",
    )
    .await?;
    for row in sqlx::query(
        "SELECT COALESCE(disconnect_reason, 'unknown') AS reason, COUNT(*) AS count \
         FROM sessions WHERE disconnected_at IS NOT NULL GROUP BY COALESCE(disconnect_reason, 'unknown')",
    )
    .fetch_all(&state.pool)
    .await?
    {
        push_labeled_gauge(
            &mut out,
            "http_tunnel_session_disconnects_total",
            &[("reason", &row.get::<String, _>("reason"))],
            row.get::<i64, _>("count"),
        );
    }
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_reconnect_tokens_accepted_total",
        "SELECT COUNT(*) AS value FROM events WHERE kind = 'client_reconnect_token_accepted'",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_reconnect_tokens_rejected_total",
        "SELECT COUNT(*) AS value FROM events WHERE kind = 'client_reconnect_token_rejected'",
    )
    .await?;
    push_query_gauge(
        &mut out,
        &state.pool,
        "http_tunnel_audit_logs_total",
        "SELECT COUNT(*) AS value FROM audit_logs",
    )
    .await?;
    Ok(out)
}

async fn push_query_gauge(
    out: &mut String,
    pool: &sqlx::SqlitePool,
    name: &str,
    query: &str,
) -> anyhow::Result<()> {
    let row = sqlx::query(query).fetch_one(pool).await?;
    push_gauge(out, name, row.get::<i64, _>("value"));
    Ok(())
}

fn push_gauge(out: &mut String, name: &str, value: i64) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" gauge\n");
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_labeled_gauge(out: &mut String, name: &str, labels: &[(&str, &str)], value: i64) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" gauge\n");
    out.push_str(name);
    out.push('{');
    for (index, (key, value)) in labels.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(key);
        out.push_str("=\"");
        out.push_str(&value.replace('"', "\\\""));
        out.push('"');
    }
    out.push_str("} ");
    out.push_str(&value.to_string());
    out.push('\n');
}
