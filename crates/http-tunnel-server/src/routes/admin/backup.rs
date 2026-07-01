use super::*;
use axum::{
    body::Body,
    extract::{connect_info::ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, Deserialize)]
pub struct RestoreValidateRequest {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct RestoreValidateResponse {
    pub validation: crate::backup::BackupValidationReport,
    pub restore_plan: Option<crate::backup::BackupRestoreReport>,
}

pub async fn backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Response> {
    let actor = require_admin_write(&state, &headers).await?;
    let config_path = PathBuf::from(&state.config_path);
    let (bytes, report) = crate::backup::create_backup_bytes(&state.pool, config_path.as_path())
        .await
        .map_err(AppError::internal)?;
    let detail = serde_json::to_string(&report).ok();
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "backup",
            target_type: Some("backup"),
            target_id: None,
            result: "success",
            detail: detail.as_deref(),
        },
    )
    .await?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"http-tunnel-backup.zip\"",
        )
        .body(Body::from(bytes))
        .map_err(AppError::internal)
}

pub async fn restore_validate(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<RestoreValidateRequest>,
) -> Result<Json<ApiResponse<RestoreValidateResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    if req.path.trim().is_empty() {
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "restore_validate",
                target_type: Some("backup"),
                target_id: None,
                result: "failure",
                detail: Some("missing backup path"),
            },
        )
        .await?;
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "missing_backup_path",
            "backup path is required",
        ));
    }
    let path = PathBuf::from(req.path.trim());
    let report = match crate::backup::validate_backup_file(path.as_path()) {
        Ok(report) => report,
        Err(error) => {
            let detail = error.to_string();
            record_admin_audit(
                &state,
                &headers,
                remote_addr,
                AuditLog {
                    actor_token: Some(&actor),
                    action: "restore_validate",
                    target_type: Some("backup"),
                    target_id: Some(req.path.trim()),
                    result: "failure",
                    detail: Some(&detail),
                },
            )
            .await?;
            return Err(AppError::internal(error));
        }
    };
    let config_path = PathBuf::from(&state.config_path);
    let restore_plan = if report.valid {
        Some(
            crate::backup::restore_backup_file(path.as_path(), config_path.as_path(), true)
                .map_err(AppError::internal)?,
        )
    } else {
        None
    };
    let response = RestoreValidateResponse {
        validation: report,
        restore_plan,
    };
    let detail = serde_json::to_string(&response).ok();
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "restore_validate",
            target_type: Some("backup"),
            target_id: Some(response.validation.path.as_str()),
            result: if response.validation.valid {
                "success"
            } else {
                "failure"
            },
            detail: detail.as_deref(),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(response)))
}
