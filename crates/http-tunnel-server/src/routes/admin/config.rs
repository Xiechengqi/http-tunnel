use super::*;
use crate::net::cidr_is_valid;
use axum::{extract::connect_info::ConnectInfo, extract::State, http::HeaderMap, Json};
use http_tunnel_common::{
    api::ApiResponse,
    config::{default_data_dir, default_database_url},
    token::{generate_token, hash_token},
    ServerConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;

pub async fn get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Value>>> {
    require_admin(&state, &headers).await?;
    let cfg = state.config.read().await;
    Ok(Json(ApiResponse::ok(public_config_value(&cfg)?)))
}

pub async fn config_schema(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Vec<ConfigFieldSchema>>>> {
    require_admin(&state, &headers).await?;
    Ok(Json(ApiResponse::ok(config_schema_entries())))
}

pub async fn put_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(new_cfg): Json<ServerConfig>,
) -> Result<Json<ApiResponse<ConfigUpdateResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let current_cfg = state.config.read().await.clone();
    let new_cfg = merge_preserved_secrets(new_cfg, &current_cfg);
    let errors = validate_server_config(&new_cfg);
    if !errors.is_empty() {
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "config_update",
                target_type: Some("config"),
                target_id: None,
                result: "failure",
                detail: Some("validation failed"),
            },
        )
        .await?;
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_config",
            errors.join("; "),
        ));
    }
    let mut cfg = state.config.write().await;
    let pending_restart = current_cfg.addr != new_cfg.addr
        || current_cfg.domain != new_cfg.domain
        || current_cfg.public_scheme != new_cfg.public_scheme
        || current_cfg.database_url != new_cfg.database_url
        || current_cfg.data_dir != new_cfg.data_dir;
    *cfg = new_cfg;
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    if pending_restart {
        set_pending_restart(&state, true).await?;
    }
    add_audit_event(
        &state,
        "admin_config_updated",
        Some(if pending_restart {
            "restart required"
        } else {
            "hot reloadable"
        }),
    )
    .await?;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "config_update",
            target_type: Some("config"),
            target_id: None,
            result: "success",
            detail: Some(if pending_restart {
                "restart required"
            } else {
                "hot reloadable"
            }),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(ConfigUpdateResponse {
        pending_restart,
    })))
}

#[derive(Debug, Serialize)]
pub struct ConfigUpdateResponse {
    pub pending_restart: bool,
}

pub async fn validate_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(new_cfg): Json<ServerConfig>,
) -> Result<Json<ApiResponse<ConfigValidationResponse>>> {
    require_admin_write(&state, &headers).await?;
    let current_cfg = state.config.read().await.clone();
    let cfg = merge_preserved_secrets(new_cfg, &current_cfg);
    let errors = validate_server_config(&cfg);
    Ok(Json(ApiResponse::ok(ConfigValidationResponse {
        valid: errors.is_empty(),
        errors,
    })))
}

#[derive(Debug, Serialize)]
pub struct ConfigValidationResponse {
    pub valid: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigFieldSchema {
    pub key: &'static str,
    pub category: &'static str,
    pub env: &'static str,
    pub value_type: &'static str,
    pub secret: bool,
    pub required: bool,
    pub restart_required: bool,
    pub hot_reloadable: bool,
    pub default: String,
    pub allowed_values: &'static [&'static str],
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub description: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TunnelCreateTokenResponse {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct SecretUpdateRequest {
    pub secret: String,
}

#[derive(Debug, Serialize)]
pub struct SecretConfiguredResponse {
    pub configured: bool,
}

pub async fn reload_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<Value>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let cfg = ServerConfig::load(&state.config_path).map_err(AppError::internal)?;
    *state.config.write().await = cfg.clone();
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "config_reload",
            target_type: Some("config"),
            target_id: None,
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(public_config_value(&cfg)?)))
}

pub async fn rotate_tunnel_create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<TunnelCreateTokenResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let token = generate_token();
    let mut cfg = state.config.write().await;
    cfg.tunnel_create_bearer_token_hash = Some(hash_token(&token));
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "tunnel_create_token_rotate",
            target_type: Some("config"),
            target_id: Some("tunnel_create_bearer_token_hash"),
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(TunnelCreateTokenResponse { token })))
}

pub async fn clear_tunnel_create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<SecretConfiguredResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let mut cfg = state.config.write().await;
    if !cfg.public_tunnel_create_enabled {
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "create_token_required",
            "enable public tunnel creation before clearing the creation token",
        ));
    }
    cfg.tunnel_create_bearer_token_hash = None;
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "tunnel_create_token_clear",
            target_type: Some("config"),
            target_id: Some("tunnel_create_bearer_token_hash"),
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(SecretConfiguredResponse {
        configured: false,
    })))
}

pub async fn rotate_metrics_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<TunnelCreateTokenResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let token = generate_token();
    let mut cfg = state.config.write().await;
    cfg.metrics_bearer_token_hash = Some(hash_token(&token));
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "metrics_token_rotate",
            target_type: Some("config"),
            target_id: Some("metrics_bearer_token_hash"),
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(TunnelCreateTokenResponse { token })))
}

pub async fn clear_metrics_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<SecretConfiguredResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let mut cfg = state.config.write().await;
    cfg.metrics_bearer_token_hash = None;
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "metrics_token_clear",
            target_type: Some("config"),
            target_id: Some("metrics_bearer_token_hash"),
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(SecretConfiguredResponse {
        configured: false,
    })))
}

pub async fn set_turnstile_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<SecretUpdateRequest>,
) -> Result<Json<ApiResponse<SecretConfiguredResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    if req.secret.trim().is_empty() {
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "empty_secret",
            "secret must not be empty",
        ));
    }
    let mut cfg = state.config.write().await;
    cfg.turnstile_secret = Some(req.secret);
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "turnstile_secret_set",
            target_type: Some("config"),
            target_id: Some("turnstile_secret"),
            result: "success",
            detail: Some("configured=true"),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(SecretConfiguredResponse {
        configured: true,
    })))
}

pub async fn clear_turnstile_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<SecretConfiguredResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let mut cfg = state.config.write().await;
    cfg.turnstile_secret = None;
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "turnstile_secret_clear",
            target_type: Some("config"),
            target_id: Some("turnstile_secret"),
            result: "success",
            detail: Some("configured=false"),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(SecretConfiguredResponse {
        configured: false,
    })))
}

pub(crate) fn validate_server_config(cfg: &ServerConfig) -> Vec<String> {
    let mut errors = Vec::new();

    if !matches!(cfg.public_scheme.as_str(), "http" | "https") {
        errors.push("public_scheme must be either \"http\" or \"https\"".to_string());
    }
    match cfg.domain.as_deref().map(str::trim) {
        Some(domain) if !domain.is_empty() => validate_domain(domain, &mut errors),
        _ => errors.push("domain is required".to_string()),
    }
    if !cfg.database_url.starts_with("sqlite://") {
        errors.push("database_url must start with sqlite://".to_string());
    }
    if cfg.data_dir.trim().is_empty() {
        errors.push("data_dir is required".to_string());
    }
    for cidr in &cfg.trusted_proxy_cidrs {
        if !cidr_is_valid(cidr) {
            errors.push(format!(
                "trusted_proxy_cidrs contains invalid CIDR \"{cidr}\""
            ));
        }
    }
    if cfg.tunnel_ttl_seconds < 60 {
        errors.push("tunnel_ttl_seconds must be at least 60".to_string());
    }
    if cfg.reserved_ttl_seconds < 60 {
        errors.push("reserved_ttl_seconds must be at least 60".to_string());
    }
    if cfg.max_body_bytes == 0 {
        errors.push("max_body_bytes must be greater than 0".to_string());
    }
    if cfg.max_header_bytes == 0 {
        errors.push("max_header_bytes must be greater than 0".to_string());
    }
    if cfg.max_concurrent_streams == 0 {
        errors.push("max_concurrent_streams must be greater than 0".to_string());
    }
    if cfg.request_timeout_seconds == 0 {
        errors.push("request_timeout_seconds must be greater than 0".to_string());
    }
    if cfg.idle_timeout_seconds == 0 {
        errors.push("idle_timeout_seconds must be greater than 0".to_string());
    }
    if cfg.heartbeat_interval_seconds == 0 {
        errors.push("heartbeat_interval_seconds must be greater than 0".to_string());
    }
    if cfg.stale_session_seconds <= cfg.heartbeat_interval_seconds {
        errors.push(
            "stale_session_seconds must be greater than heartbeat_interval_seconds".to_string(),
        );
    }
    if !matches!(cfg.duplicate_session_policy.as_str(), "replace" | "reject") {
        errors
            .push("duplicate_session_policy must be either \"replace\" or \"reject\"".to_string());
    }
    if !matches!(
        cfg.session_pool_policy.as_str(),
        "single_replace" | "single_reject" | "round_robin" | "least_loaded"
    ) {
        errors.push(
            "session_pool_policy must be one of \"single_replace\", \"single_reject\", \"round_robin\", or \"least_loaded\""
                .to_string(),
        );
    }
    if cfg.max_ws_message_bytes == 0 {
        errors.push("max_ws_message_bytes must be greater than 0".to_string());
    }
    if cfg.inspector_max_body_preview_bytes == 0 {
        errors.push("inspector_max_body_preview_bytes must be greater than 0".to_string());
    }
    if cfg.admin_login_failure_limit == 0 {
        errors.push("admin_login_failure_limit must be greater than 0".to_string());
    }
    if cfg.admin_login_cooldown_seconds == 0 {
        errors.push("admin_login_cooldown_seconds must be greater than 0".to_string());
    }
    if !valid_http_url(&cfg.turnstile_verify_url) {
        errors.push("turnstile_verify_url must start with http:// or https://".to_string());
    }
    if cfg.cleanup_interval_seconds == 0 {
        errors.push("cleanup_interval_seconds must be greater than 0".to_string());
    }
    if cfg.request_log_retention_days == 0 {
        errors.push("request_log_retention_days must be greater than 0".to_string());
    }
    if cfg.event_retention_days == 0 {
        errors.push("event_retention_days must be greater than 0".to_string());
    }
    if cfg.session_retention_days == 0 {
        errors.push("session_retention_days must be greater than 0".to_string());
    }
    if !cfg.public_tunnel_create_enabled && cfg.tunnel_create_bearer_token_hash.is_none() {
        errors.push(
            "public_tunnel_create_enabled=false requires tunnel_create_bearer_token_hash"
                .to_string(),
        );
    }
    if !cfg.release_repo.trim().is_empty() && !valid_release_repo(&cfg.release_repo) {
        errors.push("release_repo must use owner/repo format".to_string());
    }
    if cfg.release_tag.trim().is_empty() {
        errors.push("release_tag is required".to_string());
    }
    for reserved in &cfg.reserved_subdomains {
        if !valid_dns_label(reserved) {
            errors.push(format!(
                "reserved_subdomains contains invalid DNS label \"{reserved}\""
            ));
        }
    }
    if cfg.allow_custom_subdomain && cfg.require_random_subdomain {
        errors.push(
            "allow_custom_subdomain has no effect while require_random_subdomain is true"
                .to_string(),
        );
    }

    errors
}

pub(crate) fn public_config_value(cfg: &ServerConfig) -> Result<Value> {
    let mut value = serde_json::to_value(cfg).map_err(AppError::internal)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("admin_password_hash");
        object.remove("admin_session_secret");
        object.remove("reconnect_token_secret");
        let metrics_token_configured = object
            .remove("metrics_bearer_token_hash")
            .is_some_and(|value| value.as_str().is_some_and(|hash| !hash.is_empty()));
        let turnstile_configured = object
            .remove("turnstile_secret")
            .is_some_and(|value| value.as_str().is_some_and(|secret| !secret.is_empty()));
        let create_token_configured = object
            .remove("tunnel_create_bearer_token_hash")
            .is_some_and(|value| value.as_str().is_some_and(|hash| !hash.is_empty()));
        object.insert(
            "turnstile_configured".to_string(),
            Value::Bool(turnstile_configured),
        );
        object.insert(
            "metrics_bearer_token_configured".to_string(),
            Value::Bool(metrics_token_configured),
        );
        object.insert(
            "tunnel_create_bearer_token_configured".to_string(),
            Value::Bool(create_token_configured),
        );
    }
    Ok(value)
}

pub(crate) fn config_schema_entries() -> Vec<ConfigFieldSchema> {
    vec![
        schema(
            "domain",
            "Core",
            "HTTP_TUNNEL_DOMAIN",
            false,
            true,
            "",
            "Public root domain used for subdomain routing.",
        ),
        schema(
            "public_scheme",
            "Core",
            "HTTP_TUNNEL_PUBLIC_SCHEME",
            false,
            true,
            "https",
            "Public URL scheme advertised to clients and cookies.",
        ),
        schema(
            "addr",
            "Core",
            "HTTP_TUNNEL_ADDR",
            false,
            true,
            "0.0.0.0:8080",
            "Server listen address.",
        ),
        schema(
            "trust_proxy_headers",
            "Core",
            "HTTP_TUNNEL_TRUST_PROXY_HEADERS",
            false,
            false,
            "true",
            "Trust forwarded client IP headers only from trusted proxy CIDRs.",
        ),
        schema(
            "trusted_proxy_cidrs",
            "Core",
            "HTTP_TUNNEL_TRUSTED_PROXY_CIDRS",
            false,
            false,
            "127.0.0.1/32,::1/128",
            "CIDRs allowed to supply X-Forwarded-For.",
        ),
        schema(
            "database_url",
            "Storage",
            "HTTP_TUNNEL_DATABASE_URL",
            false,
            true,
            default_database_url(),
            "SQLite database URL.",
        ),
        schema(
            "data_dir",
            "Storage",
            "HTTP_TUNNEL_DATA_DIR",
            false,
            true,
            default_data_dir(),
            "Directory used for local runtime data and backups.",
        ),
        schema(
            "tunnel_ttl_seconds",
            "Tunnel Limits",
            "HTTP_TUNNEL_TUNNEL_TTL_SECONDS",
            false,
            false,
            "86400",
            "Default connected tunnel TTL.",
        ),
        schema(
            "reserved_ttl_seconds",
            "Tunnel Limits",
            "HTTP_TUNNEL_RESERVED_TTL_SECONDS",
            false,
            false,
            "300",
            "TTL for reserved tunnels before a client connects.",
        ),
        schema(
            "max_body_bytes",
            "Tunnel Limits",
            "HTTP_TUNNEL_MAX_BODY_BYTES",
            false,
            false,
            "26214400",
            "Maximum proxied HTTP request body size.",
        ),
        schema(
            "max_header_bytes",
            "Tunnel Limits",
            "HTTP_TUNNEL_MAX_HEADER_BYTES",
            false,
            false,
            "65536",
            "Maximum public request header bytes.",
        ),
        schema(
            "max_concurrent_streams",
            "Tunnel Limits",
            "HTTP_TUNNEL_MAX_CONCURRENT_STREAMS",
            false,
            false,
            "128",
            "Maximum simultaneous tunnel streams.",
        ),
        schema(
            "request_timeout_seconds",
            "Tunnel Limits",
            "HTTP_TUNNEL_REQUEST_TIMEOUT_SECONDS",
            false,
            false,
            "60",
            "HTTP request timeout through the tunnel.",
        ),
        schema(
            "idle_timeout_seconds",
            "Tunnel Limits",
            "HTTP_TUNNEL_IDLE_TIMEOUT_SECONDS",
            false,
            false,
            "300",
            "Idle timeout for public WebSocket sessions.",
        ),
        schema(
            "heartbeat_interval_seconds",
            "Tunnel Limits",
            "HTTP_TUNNEL_HEARTBEAT_INTERVAL_SECONDS",
            false,
            false,
            "15",
            "Protocol heartbeat interval.",
        ),
        schema(
            "stale_session_seconds",
            "Tunnel Limits",
            "HTTP_TUNNEL_STALE_SESSION_SECONDS",
            false,
            false,
            "45",
            "Disconnect sessions with no heartbeat activity after this duration.",
        ),
        schema(
            "duplicate_session_policy",
            "Tunnel Limits",
            "HTTP_TUNNEL_DUPLICATE_SESSION_POLICY",
            false,
            false,
            "replace",
            "Legacy duplicate-session policy.",
        ),
        schema(
            "session_pool_policy",
            "Tunnel Limits",
            "HTTP_TUNNEL_SESSION_POOL_POLICY",
            false,
            false,
            "single_replace",
            "Session selection policy for one or more active clients.",
        ),
        schema(
            "max_ws_message_bytes",
            "Tunnel Limits",
            "HTTP_TUNNEL_MAX_WS_MESSAGE_BYTES",
            false,
            false,
            "8388608",
            "Maximum public WebSocket message size.",
        ),
        schema(
            "cleanup_interval_seconds",
            "Retention",
            "HTTP_TUNNEL_CLEANUP_INTERVAL_SECONDS",
            false,
            false,
            "60",
            "Background cleanup interval.",
        ),
        schema(
            "request_log_retention_days",
            "Retention",
            "HTTP_TUNNEL_REQUEST_LOG_RETENTION_DAYS",
            false,
            false,
            "30",
            "Request log retention window.",
        ),
        schema(
            "event_retention_days",
            "Retention",
            "HTTP_TUNNEL_EVENT_RETENTION_DAYS",
            false,
            false,
            "90",
            "Event and audit retention window.",
        ),
        schema(
            "session_retention_days",
            "Retention",
            "HTTP_TUNNEL_SESSION_RETENTION_DAYS",
            false,
            false,
            "30",
            "Session row retention window.",
        ),
        schema(
            "inspector_enabled_default",
            "Inspector",
            "HTTP_TUNNEL_INSPECTOR_ENABLED_DEFAULT",
            false,
            false,
            "false",
            "Enable request inspector on newly created tunnels by default.",
        ),
        schema(
            "inspector_max_body_preview_bytes",
            "Inspector",
            "HTTP_TUNNEL_INSPECTOR_MAX_BODY_PREVIEW_BYTES",
            false,
            false,
            "16384",
            "Maximum body preview bytes captured by Inspector.",
        ),
        schema(
            "admin_login_failure_limit",
            "Security",
            "HTTP_TUNNEL_ADMIN_LOGIN_FAILURE_LIMIT",
            false,
            false,
            "10",
            "Failed admin login attempts allowed per cooldown window.",
        ),
        schema(
            "admin_login_cooldown_seconds",
            "Security",
            "HTTP_TUNNEL_ADMIN_LOGIN_COOLDOWN_SECONDS",
            false,
            false,
            "60",
            "Admin login rate-limit cooldown window.",
        ),
        schema(
            "rate_limit_per_ip",
            "Security",
            "HTTP_TUNNEL_RATE_LIMIT_PER_IP",
            false,
            false,
            "60",
            "Anonymous tunnel creation rate limit per IP.",
        ),
        schema(
            "per_tunnel_rate_limit_per_minute",
            "Security",
            "HTTP_TUNNEL_PER_TUNNEL_RATE_LIMIT_PER_MINUTE",
            false,
            false,
            "0",
            "Default per-tunnel public request rate limit; zero disables it.",
        ),
        schema(
            "metrics_public",
            "Observability",
            "HTTP_TUNNEL_METRICS_PUBLIC",
            false,
            false,
            "false",
            "Allow unauthenticated access to /metrics.",
        ),
        schema(
            "metrics_bearer_token_hash",
            "Observability",
            "HTTP_TUNNEL_METRICS_BEARER_TOKEN_HASH",
            true,
            false,
            "",
            "Hash for an optional dedicated /metrics bearer token.",
        ),
        schema(
            "public_tunnel_create_enabled",
            "Security",
            "HTTP_TUNNEL_PUBLIC_TUNNEL_CREATE_ENABLED",
            false,
            false,
            "true",
            "Allow public anonymous tunnel creation.",
        ),
        schema(
            "tunnel_create_bearer_token_hash",
            "Security",
            "HTTP_TUNNEL_CREATE_BEARER_TOKEN_HASH",
            true,
            false,
            "",
            "Hash for the optional tunnel creation bearer token.",
        ),
        schema(
            "max_active_tunnels_per_ip",
            "Security",
            "HTTP_TUNNEL_MAX_ACTIVE_TUNNELS_PER_IP",
            false,
            false,
            "0",
            "Maximum active tunnels per client IP; zero disables it.",
        ),
        schema(
            "reserved_subdomains",
            "Security",
            "HTTP_TUNNEL_RESERVED_SUBDOMAINS",
            false,
            false,
            "built-in reserved names",
            "Reserved subdomains that clients cannot claim.",
        ),
        schema(
            "allow_custom_subdomain",
            "Security",
            "HTTP_TUNNEL_ALLOW_CUSTOM_SUBDOMAIN",
            false,
            false,
            "true",
            "Allow clients to request a specific subdomain.",
        ),
        schema(
            "require_random_subdomain",
            "Security",
            "HTTP_TUNNEL_REQUIRE_RANDOM_SUBDOMAIN",
            false,
            false,
            "false",
            "Force server-generated random subdomains.",
        ),
        schema(
            "turnstile_secret",
            "Security",
            "HTTP_TUNNEL_TURNSTILE_SECRET",
            true,
            false,
            "",
            "Cloudflare Turnstile secret for public tunnel creation.",
        ),
        schema(
            "turnstile_verify_url",
            "Security",
            "HTTP_TUNNEL_TURNSTILE_VERIFY_URL",
            false,
            false,
            "https://challenges.cloudflare.com/turnstile/v0/siteverify",
            "Turnstile verification endpoint.",
        ),
        schema(
            "release_repo",
            "Upgrade",
            "HTTP_TUNNEL_RELEASE_REPO",
            false,
            false,
            "",
            "Optional GitHub owner/repo used by upgrade checks.",
        ),
        schema(
            "release_tag",
            "Upgrade",
            "HTTP_TUNNEL_RELEASE_TAG",
            false,
            false,
            "latest",
            "GitHub release tag used by upgrade checks.",
        ),
        schema(
            "systemd_unit",
            "Upgrade",
            "HTTP_TUNNEL_SYSTEMD_UNIT",
            false,
            false,
            "",
            "Optional systemd unit name for restart/upgrade.",
        ),
    ]
}

fn schema(
    key: &'static str,
    category: &'static str,
    env: &'static str,
    secret: bool,
    restart_required: bool,
    default: impl Into<String>,
    description: &'static str,
) -> ConfigFieldSchema {
    let metadata = config_field_metadata(key);
    ConfigFieldSchema {
        key,
        category,
        env,
        value_type: metadata.value_type,
        secret,
        required: metadata.required,
        restart_required,
        hot_reloadable: !restart_required,
        default: default.into(),
        allowed_values: metadata.allowed_values,
        min: metadata.min,
        max: metadata.max,
        description,
    }
}

struct ConfigFieldMetadata {
    value_type: &'static str,
    required: bool,
    allowed_values: &'static [&'static str],
    min: Option<i64>,
    max: Option<i64>,
}

fn config_field_metadata(key: &str) -> ConfigFieldMetadata {
    let mut metadata = ConfigFieldMetadata {
        value_type: "string",
        required: matches!(
            key,
            "domain" | "public_scheme" | "addr" | "database_url" | "data_dir" | "release_tag"
        ),
        allowed_values: &[],
        min: None,
        max: None,
    };
    metadata.value_type = match key {
        "domain" => "hostname",
        "public_scheme" => {
            metadata.allowed_values = &["http", "https"];
            "enum"
        }
        "addr" => "socket_addr",
        "database_url" => "sqlite_url",
        "trust_proxy_headers"
        | "inspector_enabled_default"
        | "metrics_public"
        | "public_tunnel_create_enabled"
        | "allow_custom_subdomain"
        | "require_random_subdomain" => {
            metadata.allowed_values = &["false", "true"];
            "bool"
        }
        "trusted_proxy_cidrs" => "cidr_list",
        "reserved_subdomains" => "dns_label_list",
        "duplicate_session_policy" => {
            metadata.allowed_values = &["replace", "reject"];
            "enum"
        }
        "session_pool_policy" => {
            metadata.allowed_values = &[
                "single_replace",
                "single_reject",
                "round_robin",
                "least_loaded",
            ];
            "enum"
        }
        "turnstile_verify_url" => "url",
        "release_repo" => "github_repo",
        "systemd_unit" => "optional_string",
        "turnstile_secret" | "tunnel_create_bearer_token_hash" | "metrics_bearer_token_hash" => {
            "secret"
        }
        "tunnel_ttl_seconds" | "reserved_ttl_seconds" => {
            metadata.min = Some(60);
            "integer"
        }
        "max_body_bytes"
        | "max_header_bytes"
        | "max_concurrent_streams"
        | "request_timeout_seconds"
        | "idle_timeout_seconds"
        | "heartbeat_interval_seconds"
        | "stale_session_seconds"
        | "max_ws_message_bytes"
        | "cleanup_interval_seconds"
        | "request_log_retention_days"
        | "event_retention_days"
        | "session_retention_days"
        | "inspector_max_body_preview_bytes"
        | "admin_login_failure_limit"
        | "admin_login_cooldown_seconds" => {
            metadata.min = Some(1);
            "integer"
        }
        "rate_limit_per_ip" | "per_tunnel_rate_limit_per_minute" | "max_active_tunnels_per_ip" => {
            metadata.min = Some(0);
            "integer"
        }
        _ => "string",
    };
    metadata
}

fn merge_preserved_secrets(mut new_cfg: ServerConfig, current_cfg: &ServerConfig) -> ServerConfig {
    if new_cfg.admin_password_hash.is_none() {
        new_cfg.admin_password_hash = current_cfg.admin_password_hash.clone();
    }
    if new_cfg.admin_session_secret.is_none() {
        new_cfg.admin_session_secret = current_cfg.admin_session_secret.clone();
    }
    if new_cfg.reconnect_token_secret.is_none() {
        new_cfg.reconnect_token_secret = current_cfg.reconnect_token_secret.clone();
    }
    if new_cfg.metrics_bearer_token_hash.is_none() {
        new_cfg.metrics_bearer_token_hash = current_cfg.metrics_bearer_token_hash.clone();
    }
    if new_cfg.turnstile_secret.is_none() {
        new_cfg.turnstile_secret = current_cfg.turnstile_secret.clone();
    }
    if new_cfg.tunnel_create_bearer_token_hash.is_none() {
        new_cfg.tunnel_create_bearer_token_hash =
            current_cfg.tunnel_create_bearer_token_hash.clone();
    }
    new_cfg
}

fn validate_domain(domain: &str, errors: &mut Vec<String>) {
    if domain.contains("://") || domain.contains('/') || domain.contains(':') {
        errors.push("domain must be a hostname without scheme, path, or port".to_string());
        return;
    }
    if domain.starts_with('.') || domain.ends_with('.') {
        errors.push("domain must not start or end with a dot".to_string());
        return;
    }
    for label in domain.split('.') {
        if !valid_dns_label(label) {
            errors.push("domain labels must be valid DNS labels".to_string());
            return;
        }
    }
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_release_repo(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(repo) = parts.next() else {
        return false;
    };
    parts.next().is_none() && valid_repo_part(owner) && valid_repo_part(repo)
}

fn valid_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn valid_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_reports_missing_domain_only() {
        let cfg = ServerConfig::default();
        let errors = validate_server_config(&cfg);
        assert_eq!(errors, vec!["domain is required"]);
    }

    #[test]
    fn validation_reports_invalid_core_fields() {
        let cfg = ServerConfig {
            domain: Some("https://bad.example.com:8080/path".to_string()),
            public_scheme: "ftp".to_string(),
            database_url: "postgres://localhost/db".to_string(),
            data_dir: String::new(),
            trusted_proxy_cidrs: vec!["127.0.0.1/33".to_string()],
            max_concurrent_streams: 0,
            request_timeout_seconds: 0,
            heartbeat_interval_seconds: 0,
            stale_session_seconds: 0,
            release_repo: "owner/repo/extra".to_string(),
            reserved_subdomains: vec!["Bad_Value".to_string()],
            ..ServerConfig::default()
        };

        let errors = validate_server_config(&cfg);

        assert!(errors.iter().any(|error| error.contains("public_scheme")));
        assert!(errors.iter().any(|error| error.contains("domain")));
        assert!(errors.iter().any(|error| error.contains("database_url")));
        assert!(errors.iter().any(|error| error.contains("data_dir")));
        assert!(errors
            .iter()
            .any(|error| error.contains("trusted_proxy_cidrs")));
        assert!(errors
            .iter()
            .any(|error| error.contains("max_concurrent_streams")));
        assert!(errors
            .iter()
            .any(|error| error.contains("request_timeout_seconds")));
        assert!(errors
            .iter()
            .any(|error| error.contains("heartbeat_interval_seconds")));
        assert!(errors
            .iter()
            .any(|error| error.contains("stale_session_seconds")));
        assert!(errors.iter().any(|error| error.contains("release_repo")));
        assert!(errors
            .iter()
            .any(|error| error.contains("reserved_subdomains")));
    }

    #[test]
    fn config_schema_reports_types_allowed_values_and_ranges() {
        let schema = config_schema_entries();
        let public_scheme = schema
            .iter()
            .find(|field| field.key == "public_scheme")
            .expect("public_scheme schema");
        assert_eq!(public_scheme.value_type, "enum");
        assert_eq!(public_scheme.allowed_values, ["http", "https"]);
        assert!(public_scheme.required);
        assert!(public_scheme.restart_required);

        let ttl = schema
            .iter()
            .find(|field| field.key == "tunnel_ttl_seconds")
            .expect("tunnel ttl schema");
        assert_eq!(ttl.value_type, "integer");
        assert_eq!(ttl.min, Some(60));
        assert!(ttl.hot_reloadable);

        let release_repo = schema
            .iter()
            .find(|field| field.key == "release_repo")
            .expect("release repo schema");
        assert!(!release_repo.required);
        assert_eq!(release_repo.default, "");
    }
}
