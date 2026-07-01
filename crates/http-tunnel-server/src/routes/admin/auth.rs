use super::*;
use crate::net::client_ip;
use axum::{
    extract::{connect_info::ConnectInfo, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use http_tunnel_common::{
    api::ApiResponse,
    ids::generate_admin_session_id,
    password::{hash_password, verify_password},
    token::{generate_token, hash_token},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::{
    net::SocketAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const ADMIN_SESSION_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 7);

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct AdminSessionRecord {
    pub id: String,
    pub remote_ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub last_seen_at: String,
    pub revoked_at: Option<String>,
    pub active: bool,
    pub current: bool,
}

#[derive(Debug, Serialize)]
pub struct RevokeAllSessionsResponse {
    pub revoked: u64,
}

#[derive(Debug, Deserialize)]
pub struct PasswordChangeRequest {
    pub current_password: String,
    pub new_password: String,
    pub confirm_password: String,
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> Result<Response> {
    let cfg = state.config.read().await;
    let Some(hash) = cfg.admin_password_hash.clone() else {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "setup_required",
            "setup is required",
        ));
    };
    let trust_proxy_headers = cfg.trust_proxy_headers;
    let trusted_proxy_cidrs = cfg.trusted_proxy_cidrs.clone();
    let session_secret = cfg.admin_session_secret.clone();
    let secure_cookie = cfg.public_scheme == "https";
    let failure_limit = cfg.admin_login_failure_limit;
    let cooldown = Duration::from_secs(cfg.admin_login_cooldown_seconds.max(1));
    drop(cfg);
    enforce_admin_login_rate_limit(
        &state,
        &headers,
        remote_addr,
        trust_proxy_headers,
        &trusted_proxy_cidrs,
        failure_limit,
        cooldown,
    )
    .await?;
    if !verify_password(&req.password, &hash) {
        let _ = add_audit_event(&state, "admin_login_failed", None).await;
        let _ = record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: None,
                action: "login",
                target_type: Some("admin"),
                target_id: None,
                result: "failure",
                detail: Some("invalid password"),
            },
        )
        .await;
        return Err(AppError::unauthorized());
    }

    let token = generate_token();
    let csrf = generate_token();
    let expires_at = SystemTime::now() + ADMIN_SESSION_TTL;
    let remote_ip = client_ip(
        &headers,
        remote_addr,
        trust_proxy_headers,
        &trusted_proxy_cidrs,
    );
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    store_admin_session(&state, &token, Some(&remote_ip), user_agent.as_deref()).await?;
    state
        .admin_tokens
        .write()
        .await
        .insert(token.clone(), expires_at);
    let mut response = Json(ApiResponse::ok(LoginResponse {
        token: token.clone(),
    }))
    .into_response();
    if let Some(secret) = session_secret {
        let secure = if secure_cookie { "; Secure" } else { "" };
        let cookie = format!(
            "http_tunnel_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800{secure}",
            sign_session_cookie(&token, &secret, expires_at)
        );
        if let Ok(value) = header::HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        let csrf_cookie =
            format!("http_tunnel_csrf={csrf}; Path=/; SameSite=Lax; Max-Age=604800{secure}");
        if let Ok(value) = header::HeaderValue::from_str(&csrf_cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    let _ = add_audit_event(&state, "admin_login", None).await;
    let _ = record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&token),
            action: "login",
            target_type: Some("admin"),
            target_id: None,
            result: "success",
            detail: None,
        },
    )
    .await;
    Ok(response)
}

async fn enforce_admin_login_rate_limit(
    state: &AppState,
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_proxy_headers: bool,
    trusted_proxy_cidrs: &[String],
    failure_limit: usize,
    cooldown: Duration,
) -> Result<()> {
    let ip = client_ip(
        headers,
        remote_addr,
        trust_proxy_headers,
        trusted_proxy_cidrs,
    );
    let now = Instant::now();
    let mut hits = state.admin_login_hits.write().await;
    let bucket = hits.entry(ip.clone()).or_default();
    while bucket
        .front()
        .is_some_and(|seen| now.duration_since(*seen) > cooldown)
    {
        bucket.pop_front();
    }
    if bucket.len() >= failure_limit {
        let _ = add_audit_event(state, "admin_login_rate_limited", Some(&ip)).await;
        let _ = record_admin_audit(
            state,
            headers,
            remote_addr,
            AuditLog {
                actor_token: None,
                action: "login",
                target_type: Some("admin"),
                target_id: None,
                result: "failure",
                detail: Some("rate limited"),
            },
        )
        .await;
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many login attempts",
        ));
    }
    bucket.push_back(now);
    Ok(())
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Response> {
    let token = require_admin_write(&state, &headers).await?;
    state.admin_tokens.write().await.remove(&token);
    revoke_admin_token(&state, &token).await?;
    let _ = add_audit_event(&state, "admin_logout", None).await;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&token),
            action: "logout",
            target_type: Some("admin"),
            target_id: None,
            result: "success",
            detail: None,
        },
    )
    .await?;
    let mut response = Json(ApiResponse::ok(())).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_static(
            "http_tunnel_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        ),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        header::HeaderValue::from_static("http_tunnel_csrf=; Path=/; SameSite=Lax; Max-Age=0"),
    );
    Ok(response)
}

pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<PasswordChangeRequest>,
) -> Result<Json<ApiResponse<()>>> {
    let actor = require_admin_write(&state, &headers).await?;
    if req.new_password.len() < 8 {
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "password_change",
                target_type: Some("admin"),
                target_id: None,
                result: "failure",
                detail: Some("weak password"),
            },
        )
        .await?;
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "new password must be at least 8 characters",
        ));
    }
    if req.new_password != req.confirm_password {
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "password_change",
                target_type: Some("admin"),
                target_id: None,
                result: "failure",
                detail: Some("password mismatch"),
            },
        )
        .await?;
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "password_mismatch",
            "password confirmation does not match",
        ));
    }
    let mut cfg = state.config.write().await;
    let Some(current_hash) = cfg.admin_password_hash.clone() else {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "setup_required",
            "setup is required",
        ));
    };
    if !verify_password(&req.current_password, &current_hash) {
        drop(cfg);
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "password_change",
                target_type: Some("admin"),
                target_id: None,
                result: "failure",
                detail: Some("invalid current password"),
            },
        )
        .await?;
        return Err(AppError::unauthorized());
    }
    cfg.admin_password_hash = Some(hash_password(&req.new_password).map_err(AppError::internal)?);
    cfg.save(&state.config_path).map_err(AppError::internal)?;
    drop(cfg);
    add_audit_event(&state, "admin_password_changed", None).await?;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "password_change",
            target_type: Some("admin"),
            target_id: None,
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(())))
}

pub async fn sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Vec<AdminSessionRecord>>>> {
    let token = require_admin(&state, &headers).await?;
    let current_hash = hash_token(&token);
    let rows = sqlx::query(
        "SELECT id, token_hash, remote_ip, user_agent, created_at, expires_at, last_seen_at, revoked_at, \
         revoked_at IS NULL AND expires_at > CURRENT_TIMESTAMP AS active \
         FROM admin_sessions ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(Json(ApiResponse::ok(
        rows.into_iter()
            .map(|row| AdminSessionRecord {
                id: row.get::<String, _>("id"),
                remote_ip: row.try_get::<Option<String>, _>("remote_ip").ok().flatten(),
                user_agent: row
                    .try_get::<Option<String>, _>("user_agent")
                    .ok()
                    .flatten(),
                created_at: row.get::<String, _>("created_at"),
                expires_at: row.get::<String, _>("expires_at"),
                last_seen_at: row.get::<String, _>("last_seen_at"),
                revoked_at: row
                    .try_get::<Option<String>, _>("revoked_at")
                    .ok()
                    .flatten(),
                active: row.get::<bool, _>("active"),
                current: row.get::<String, _>("token_hash") == current_hash,
            })
            .collect(),
    )))
}

pub async fn revoke_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let token_hash = sqlx::query("SELECT token_hash FROM admin_sessions WHERE id = ?1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?
        .map(|row| row.get::<String, _>("token_hash"))
        .ok_or_else(|| {
            AppError::new(
                StatusCode::NOT_FOUND,
                "admin_session_not_found",
                "admin session not found",
            )
        })?;
    sqlx::query(
        "UPDATE admin_sessions SET revoked_at = COALESCE(revoked_at, CURRENT_TIMESTAMP) WHERE id = ?1",
    )
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    remove_admin_token_hash(&state, &token_hash).await;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "admin_session_revoke",
            target_type: Some("admin_session"),
            target_id: Some(&id),
            result: "success",
            detail: None,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(())))
}

pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<RevokeAllSessionsResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let current_hash = hash_token(&actor);
    let result = sqlx::query(
        "UPDATE admin_sessions SET revoked_at = CURRENT_TIMESTAMP \
         WHERE revoked_at IS NULL AND expires_at > CURRENT_TIMESTAMP AND token_hash != ?1",
    )
    .bind(&current_hash)
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    remove_admin_tokens_except_hash(&state, &current_hash).await;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "admin_session_revoke_all",
            target_type: Some("admin_session"),
            target_id: None,
            result: "success",
            detail: Some("revoked all active sessions except current"),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(RevokeAllSessionsResponse {
        revoked: result.rows_affected(),
    })))
}

pub async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<String> {
    if let Some(token) = bearer_token(headers) {
        if bearer_token_valid(state, &token).await {
            return Ok(token);
        }
    }

    if let Some(cookie) = session_cookie(headers) {
        let cfg = state.config.read().await;
        if let Some(secret) = cfg.admin_session_secret.as_deref() {
            if let Some(token) = verify_session_cookie(&cookie, secret) {
                if bearer_token_valid(state, &token).await {
                    return Ok(token);
                }
            }
        }
    }

    Err(AppError::unauthorized())
}

pub async fn require_admin_write(state: &AppState, headers: &HeaderMap) -> Result<String> {
    if let Some(token) = bearer_token(headers) {
        if bearer_token_valid(state, &token).await {
            return Ok(token);
        }
    }

    if let Some(cookie) = session_cookie(headers) {
        let cfg = state.config.read().await;
        if let Some(secret) = cfg.admin_session_secret.as_deref() {
            if let Some(token) = verify_session_cookie(&cookie, secret) {
                if !bearer_token_valid(state, &token).await {
                    return Err(AppError::unauthorized());
                }
                if csrf_valid(headers) {
                    return Ok(token);
                }
                let _ = record_admin_audit_without_remote(
                    state,
                    AuditLog {
                        actor_token: Some(&token),
                        action: "csrf_check",
                        target_type: Some("admin"),
                        target_id: None,
                        result: "failure",
                        detail: Some("missing or invalid CSRF token"),
                    },
                )
                .await;
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "csrf_required",
                    "missing or invalid CSRF token",
                ));
            }
        }
    }

    Err(AppError::unauthorized())
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(ToString::to_string)
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, "http_tunnel_session")
}

fn csrf_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, "http_tunnel_csrf")
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&prefix))
        .map(ToString::to_string)
}

fn csrf_valid(headers: &HeaderMap) -> bool {
    let Some(cookie) = csrf_cookie(headers) else {
        return false;
    };
    headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|header| header == cookie)
}

pub(super) async fn bearer_token_valid(state: &AppState, token: &str) -> bool {
    let now = SystemTime::now();
    let mut tokens = state.admin_tokens.write().await;
    tokens.retain(|_, expires_at| *expires_at > now);
    let cached = tokens
        .get(token)
        .is_some_and(|expires_at| *expires_at > now);
    drop(tokens);
    let valid = refresh_admin_session(state, token).await.unwrap_or(false);
    if !valid && cached {
        state.admin_tokens.write().await.remove(token);
    }
    valid
}

async fn store_admin_session(
    state: &AppState,
    token: &str,
    remote_ip: Option<&str>,
    user_agent: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO admin_sessions (id, token_hash, remote_ip, user_agent, expires_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now', ?5))",
    )
    .bind(generate_admin_session_id())
    .bind(hash_token(token))
    .bind(remote_ip)
    .bind(user_agent)
    .bind(format!("+{} seconds", ADMIN_SESSION_TTL.as_secs()))
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

async fn refresh_admin_session(state: &AppState, token: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE admin_sessions SET last_seen_at = CURRENT_TIMESTAMP \
         WHERE token_hash = ?1 AND revoked_at IS NULL AND expires_at > CURRENT_TIMESTAMP",
    )
    .bind(hash_token(token))
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(result.rows_affected() > 0)
}

async fn revoke_admin_token(state: &AppState, token: &str) -> Result<()> {
    sqlx::query(
        "UPDATE admin_sessions SET revoked_at = COALESCE(revoked_at, CURRENT_TIMESTAMP) WHERE token_hash = ?1",
    )
    .bind(hash_token(token))
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

async fn remove_admin_token_hash(state: &AppState, token_hash: &str) {
    state
        .admin_tokens
        .write()
        .await
        .retain(|token, _| hash_token(token) != token_hash);
}

async fn remove_admin_tokens_except_hash(state: &AppState, current_hash: &str) {
    state
        .admin_tokens
        .write()
        .await
        .retain(|token, _| hash_token(token) == current_hash);
}

pub(super) fn sign_session_cookie(token: &str, secret: &str, expires_at: SystemTime) -> String {
    let expires_unix = expires_at
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let signature =
        http_tunnel_common::token::hash_token(&format!("{secret}:{token}:{expires_unix}"));
    format!("{token}.{expires_unix}.{signature}")
}

pub(super) fn verify_session_cookie(cookie: &str, secret: &str) -> Option<String> {
    let (prefix, signature) = cookie.rsplit_once('.')?;
    let (token, expires_unix) = prefix.rsplit_once('.')?;
    let expires_unix = expires_unix.parse::<u64>().ok()?;
    let expires_at = UNIX_EPOCH + Duration::from_secs(expires_unix);
    if expires_at <= SystemTime::now() {
        return None;
    }
    let expected =
        http_tunnel_common::token::hash_token(&format!("{secret}:{token}:{expires_unix}"));
    if signature == expected {
        Some(token.to_string())
    } else {
        None
    }
}
