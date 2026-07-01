use super::*;
use axum::{extract::State, http::HeaderMap, Json};
use http_tunnel_common::api::ApiResponse;
use serde::Serialize;
use sqlx::Row;

#[derive(Debug, Serialize)]
pub struct AdminAlert {
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
    pub tunnel_id: Option<String>,
    pub count: Option<i64>,
}

pub async fn alerts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Vec<AdminAlert>>>> {
    require_admin(&state, &headers).await?;
    Ok(Json(ApiResponse::ok(build_alerts(&state).await?)))
}

pub(crate) async fn build_alerts(state: &AppState) -> Result<Vec<AdminAlert>> {
    let mut alerts = Vec::new();
    push_connected_without_runtime(state, &mut alerts).await?;
    push_status_counts(state, &mut alerts).await?;
    push_recent_error_counts(state, &mut alerts).await?;
    push_stale_runtime_sessions(state, &mut alerts).await;
    Ok(alerts)
}

async fn push_connected_without_runtime(
    state: &AppState,
    alerts: &mut Vec<AdminAlert>,
) -> Result<()> {
    let mut active_ids = state.active_tunnel_ids().await;
    active_ids.sort();
    active_ids.dedup();
    let rows = sqlx::query("SELECT id, subdomain FROM tunnels WHERE status = 'connected'")
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::internal)?;
    for row in rows {
        let tunnel_id = row.get::<String, _>("id");
        if !active_ids.iter().any(|id| id == &tunnel_id) {
            alerts.push(AdminAlert {
                severity: "warning",
                code: "connected_without_runtime_session",
                message: format!(
                    "{} is marked connected but has no active runtime session",
                    row.get::<String, _>("subdomain")
                ),
                tunnel_id: Some(tunnel_id),
                count: None,
            });
        }
    }
    Ok(())
}

async fn push_status_counts(state: &AppState, alerts: &mut Vec<AdminAlert>) -> Result<()> {
    for (status, severity, code) in [
        ("disabled", "info", "disabled_tunnels"),
        ("disconnected", "warning", "offline_tunnels"),
        ("expired", "info", "expired_tunnels"),
    ] {
        let count = count_tunnels_by_status(state, status).await?;
        if count > 0 {
            alerts.push(AdminAlert {
                severity,
                code,
                message: format!("{count} tunnel(s) are {status}"),
                tunnel_id: None,
                count: Some(count),
            });
        }
    }
    Ok(())
}

async fn push_recent_error_counts(state: &AppState, alerts: &mut Vec<AdminAlert>) -> Result<()> {
    let recent_errors = count_recent(
        state,
        "SELECT COUNT(*) AS count FROM request_logs \
         WHERE started_at >= datetime('now', '-15 minutes') AND error IS NOT NULL",
    )
    .await?;
    if recent_errors > 0 {
        alerts.push(AdminAlert {
            severity: "warning",
            code: "recent_proxy_errors",
            message: format!("{recent_errors} request error(s) in the last 15 minutes"),
            tunnel_id: None,
            count: Some(recent_errors),
        });
    }
    let recent_5xx = count_recent(
        state,
        "SELECT COUNT(*) AS count FROM request_logs \
         WHERE started_at >= datetime('now', '-15 minutes') AND status >= 500",
    )
    .await?;
    if recent_5xx > 0 {
        alerts.push(AdminAlert {
            severity: "critical",
            code: "recent_5xx",
            message: format!("{recent_5xx} 5xx response(s) in the last 15 minutes"),
            tunnel_id: None,
            count: Some(recent_5xx),
        });
    }
    let abnormal_ws = count_recent(
        state,
        "SELECT COUNT(*) AS count FROM request_logs \
         WHERE request_type = 'ws' AND ws_close_code IS NOT NULL AND ws_close_code NOT IN (1000, 1001) \
         AND started_at >= datetime('now', '-1 day')",
    )
    .await?;
    if abnormal_ws > 0 {
        alerts.push(AdminAlert {
            severity: "warning",
            code: "abnormal_websocket_closes",
            message: format!("{abnormal_ws} abnormal websocket close(s) in the last day"),
            tunnel_id: None,
            count: Some(abnormal_ws),
        });
    }
    Ok(())
}

async fn push_stale_runtime_sessions(state: &AppState, alerts: &mut Vec<AdminAlert>) {
    let cfg = state.config.read().await.clone();
    let stale_after = std::time::Duration::from_secs(cfg.stale_session_seconds.max(1));
    let now = std::time::Instant::now();
    let sessions = state
        .sessions_by_subdomain
        .read()
        .await
        .values()
        .flat_map(|pool| pool.sessions.iter().cloned())
        .collect::<Vec<_>>();
    let mut stale = 0_i64;
    for session in sessions {
        if now.duration_since(*session.last_seen.read().await) > stale_after {
            stale += 1;
        }
    }
    if stale > 0 {
        alerts.push(AdminAlert {
            severity: "critical",
            code: "stale_runtime_sessions",
            message: format!("{stale} runtime session(s) appear stale"),
            tunnel_id: None,
            count: Some(stale),
        });
    }
}

async fn count_tunnels_by_status(state: &AppState, status: &str) -> Result<i64> {
    sqlx::query("SELECT COUNT(*) AS count FROM tunnels WHERE status = ?1")
        .bind(status)
        .fetch_one(&state.pool)
        .await
        .map(|row| row.get::<i64, _>("count"))
        .map_err(AppError::internal)
}

async fn count_recent(state: &AppState, sql: &str) -> Result<i64> {
    sqlx::query(sql)
        .fetch_one(&state.pool)
        .await
        .map(|row| row.get::<i64, _>("count"))
        .map_err(AppError::internal)
}
