use super::*;
use super::{alerts, config, maintenance};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use http_tunnel_common::{api::ApiResponse, build_info::BuildInfo};
use serde::Serialize;
use sqlx::Row;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct Diagnostics {
    pub generated_at_unix: u64,
    pub status: AdminStatus,
    pub config: serde_json::Value,
    pub config_schema: Vec<config::ConfigFieldSchema>,
    pub alerts: Vec<alerts::AdminAlert>,
    pub maintenance: maintenance::MaintenanceStatus,
    pub version: BuildInfo,
    pub metrics: DiagnosticsMetrics,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsMetrics {
    pub active_sessions: usize,
    pub active_streams: usize,
    pub request_count: i64,
    pub request_error_count: i64,
    pub websocket_session_count: i64,
    pub audit_log_count: i64,
    pub stale_session_count: i64,
}

pub async fn diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Diagnostics>>> {
    require_admin(&state, &headers).await?;
    Ok(Json(ApiResponse::ok(build_diagnostics(&state).await?)))
}

pub async fn diagnostics_export(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response> {
    require_admin(&state, &headers).await?;
    let diagnostics = build_diagnostics(&state).await?;
    let bytes = serde_json::to_vec_pretty(&diagnostics).map_err(AppError::internal)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"http-tunnel-diagnostics.json\"",
        )
        .body(Body::from(bytes))
        .map_err(AppError::internal)
}

async fn build_diagnostics(state: &AppState) -> Result<Diagnostics> {
    let config = {
        let cfg = state.config.read().await;
        config::public_config_value(&cfg)?
    };
    Ok(Diagnostics {
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        status: build_admin_status(state).await?,
        config,
        config_schema: config::config_schema_entries(),
        alerts: alerts::build_alerts(state).await?,
        maintenance: maintenance::build_status(state).await?,
        version: BuildInfo::current(),
        metrics: build_diagnostics_metrics(state).await?,
    })
}

async fn build_diagnostics_metrics(state: &AppState) -> Result<DiagnosticsMetrics> {
    Ok(DiagnosticsMetrics {
        active_sessions: state.active_session_count().await,
        active_streams: state.pending_streams.read().await.len(),
        request_count: count_query(&state.pool, "SELECT COUNT(*) AS value FROM request_logs")
            .await?,
        request_error_count: count_query(
            &state.pool,
            "SELECT COUNT(*) AS value FROM request_logs WHERE error IS NOT NULL",
        )
        .await?,
        websocket_session_count: count_query(
            &state.pool,
            "SELECT COUNT(*) AS value FROM request_logs WHERE request_type = 'ws'",
        )
        .await?,
        audit_log_count: count_query(&state.pool, "SELECT COUNT(*) AS value FROM audit_logs")
            .await?,
        stale_session_count: count_query(
            &state.pool,
            "SELECT COUNT(*) AS value FROM sessions WHERE disconnect_reason = 'stale_session'",
        )
        .await?,
    })
}

async fn count_query(pool: &sqlx::SqlitePool, sql: &str) -> Result<i64> {
    sqlx::query(sql)
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>("value"))
        .map_err(AppError::internal)
}
