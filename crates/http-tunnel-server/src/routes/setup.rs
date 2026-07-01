use crate::{
    error::{AppError, Result},
    routes::admin::validate_server_config,
    state::AppState,
};
use axum::{extract::State, http::StatusCode, Json};
use http_tunnel_common::{
    api::ApiResponse, password::hash_password, token::generate_token, ServerConfig,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Serialize)]
pub struct SetupStatus {
    pub setup_required: bool,
    pub has_admin_password: bool,
    pub has_domain: bool,
    pub has_public_scheme: bool,
    pub has_database_url: bool,
}

#[derive(Debug, Deserialize)]
pub struct SetupInitRequest {
    pub admin_password: String,
    pub confirm_password: String,
    pub domain: String,
    pub public_scheme: String,
    pub addr: Option<String>,
    pub database_url: String,
    pub release_repo: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SetupInitResponse {
    pub setup_required: bool,
    pub pending_restart: bool,
}

pub async fn status(State(state): State<AppState>) -> Json<ApiResponse<SetupStatus>> {
    let cfg = state.config.read().await;
    Json(ApiResponse::ok(setup_status(&cfg)))
}

pub async fn init(
    State(state): State<AppState>,
    Json(req): Json<SetupInitRequest>,
) -> Result<Json<ApiResponse<SetupInitResponse>>> {
    let mut cfg = state.config.write().await;
    if !cfg.setup_required() {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "setup_already_complete",
            "setup has already been completed",
        ));
    }
    if req.admin_password.len() < 8 {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "admin password must be at least 8 characters",
        ));
    }
    if req.admin_password != req.confirm_password {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "password_mismatch",
            "password confirmation does not match",
        ));
    }
    if req.domain.trim().is_empty()
        || req.public_scheme.trim().is_empty()
        || req.database_url.trim().is_empty()
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "missing_required_field",
            "domain, public scheme, and database URL are required",
        ));
    }

    let old_database_url = cfg.database_url.clone();
    let mut new_cfg = cfg.clone();
    new_cfg.domain = Some(req.domain.trim().to_ascii_lowercase());
    new_cfg.public_scheme = req.public_scheme.trim().to_ascii_lowercase();
    if let Some(addr) = req.addr.as_deref().filter(|s| !s.trim().is_empty()) {
        new_cfg.addr = addr
            .parse::<SocketAddr>()
            .map_err(|e| AppError::new(StatusCode::BAD_REQUEST, "invalid_addr", e.to_string()))?;
    }
    new_cfg.database_url = req.database_url.trim().to_string();
    if let Some(repo) = req.release_repo.filter(|s| !s.trim().is_empty()) {
        new_cfg.release_repo = repo;
    }
    let errors = validate_server_config(&new_cfg);
    if !errors.is_empty() {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            errors.join("; "),
        ));
    }

    new_cfg.admin_password_hash =
        Some(hash_password(&req.admin_password).map_err(AppError::internal)?);
    new_cfg.admin_session_secret = Some(generate_token());
    new_cfg.reconnect_token_secret = Some(generate_token());
    *cfg = new_cfg;

    cfg.save(&state.config_path).map_err(AppError::internal)?;
    let pending_restart = old_database_url != cfg.database_url;
    Ok(Json(ApiResponse::ok(SetupInitResponse {
        setup_required: cfg.setup_required(),
        pending_restart,
    })))
}

fn setup_status(cfg: &ServerConfig) -> SetupStatus {
    SetupStatus {
        setup_required: cfg.setup_required(),
        has_admin_password: cfg.admin_password_hash.is_some(),
        has_domain: cfg.domain.as_deref().is_some_and(|s| !s.is_empty()),
        has_public_scheme: !cfg.public_scheme.is_empty(),
        has_database_url: !cfg.database_url.is_empty(),
    }
}
