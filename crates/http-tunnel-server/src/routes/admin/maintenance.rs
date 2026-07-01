use super::*;
use axum::{extract::connect_info::ConnectInfo, extract::State, http::HeaderMap, Json};
use serde::Serialize;
use sqlx::Row;
use std::{net::SocketAddr, path::Path};

#[derive(Debug, Serialize)]
pub struct MaintenanceStatus {
    pub database_path: Option<String>,
    pub database_size_bytes: u64,
    pub wal_size_bytes: u64,
    pub shm_size_bytes: u64,
    pub tunnel_count: i64,
    pub session_count: i64,
    pub request_log_count: i64,
    pub event_count: i64,
    pub audit_log_count: i64,
    pub admin_session_count: i64,
    pub active_runtime_sessions: usize,
}

#[derive(Debug, Serialize)]
pub struct MaintenanceOperation {
    pub operation: &'static str,
    pub ok: bool,
    pub detail: Option<String>,
}

pub async fn maintenance_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<MaintenanceStatus>>> {
    require_admin(&state, &headers).await?;
    Ok(Json(ApiResponse::ok(build_status(&state).await?)))
}

pub async fn wal_checkpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<MaintenanceOperation>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let row = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::internal)?;
    let detail = format!(
        "busy={}, log={}, checkpointed={}",
        row.get::<i64, _>(0),
        row.get::<i64, _>(1),
        row.get::<i64, _>(2)
    );
    record_maintenance_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "maintenance_wal_checkpoint",
        Some(&detail),
    )
    .await?;
    Ok(Json(ApiResponse::ok(MaintenanceOperation {
        operation: "wal_checkpoint",
        ok: true,
        detail: Some(detail),
    })))
}

pub async fn analyze(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<MaintenanceOperation>>> {
    let actor = require_admin_write(&state, &headers).await?;
    sqlx::query("ANALYZE")
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    record_maintenance_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "maintenance_analyze",
        None,
    )
    .await?;
    Ok(Json(ApiResponse::ok(MaintenanceOperation {
        operation: "analyze",
        ok: true,
        detail: None,
    })))
}

pub async fn vacuum(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<MaintenanceOperation>>> {
    let actor = require_admin_write(&state, &headers).await?;
    sqlx::query("VACUUM")
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
    record_maintenance_audit(
        &state,
        &headers,
        remote_addr,
        &actor,
        "maintenance_vacuum",
        None,
    )
    .await?;
    Ok(Json(ApiResponse::ok(MaintenanceOperation {
        operation: "vacuum",
        ok: true,
        detail: None,
    })))
}

pub(crate) async fn build_status(state: &AppState) -> Result<MaintenanceStatus> {
    let database_path = sqlite_main_path(&state.pool).await?;
    let database_size_bytes = file_size(database_path.as_deref());
    let wal_size_bytes = database_path
        .as_deref()
        .map(|path| file_size(Some(&format!("{path}-wal"))))
        .unwrap_or_default();
    let shm_size_bytes = database_path
        .as_deref()
        .map(|path| file_size(Some(&format!("{path}-shm"))))
        .unwrap_or_default();
    Ok(MaintenanceStatus {
        database_path,
        database_size_bytes,
        wal_size_bytes,
        shm_size_bytes,
        tunnel_count: count_table(&state.pool, "tunnels").await?,
        session_count: count_table(&state.pool, "sessions").await?,
        request_log_count: count_table(&state.pool, "request_logs").await?,
        event_count: count_table(&state.pool, "events").await?,
        audit_log_count: count_table(&state.pool, "audit_logs").await?,
        admin_session_count: count_table(&state.pool, "admin_sessions").await?,
        active_runtime_sessions: state.active_session_count().await,
    })
}

async fn sqlite_main_path(pool: &sqlx::SqlitePool) -> Result<Option<String>> {
    let rows = sqlx::query("PRAGMA database_list")
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(rows.into_iter().find_map(|row| {
        let name = row.get::<String, _>("name");
        let file = row.get::<String, _>("file");
        (name == "main" && !file.is_empty()).then_some(file)
    }))
}

async fn count_table(pool: &sqlx::SqlitePool, table: &str) -> Result<i64> {
    let row = sqlx::query(&format!("SELECT COUNT(*) AS count FROM {table}"))
        .fetch_one(pool)
        .await
        .map_err(AppError::internal)?;
    Ok(row.get::<i64, _>("count"))
}

fn file_size(path: Option<&str>) -> u64 {
    path.and_then(|path| std::fs::metadata(Path::new(path)).ok())
        .map(|metadata| metadata.len())
        .unwrap_or_default()
}

async fn record_maintenance_audit(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    actor: &str,
    action: &'static str,
    detail: Option<&str>,
) -> Result<()> {
    record_admin_audit(
        state,
        headers,
        remote_addr,
        AuditLog {
            actor_token: Some(actor),
            action,
            target_type: Some("maintenance"),
            target_id: None,
            result: "success",
            detail,
        },
    )
    .await
}
