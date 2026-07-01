use crate::state::{effective_session_pool_policy, ActiveSession, AppState};
use axum::{
    body::Body,
    extract::{connect_info::ConnectInfo, Path, State},
    http::{header, Request, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use http_tunnel_common::{
    api::ApiResponse, ids::generate_request_id, subdomain::normalize_subdomain, ServerConfig,
};
use http_tunnel_common::{password::verify_password, token::verify_token};
use http_tunnel_protocol::Frame;
use sqlx::Row;
use std::{collections::VecDeque, net::SocketAddr, time::Duration};

mod http;
mod pages;
mod ws;

#[derive(Debug, Clone)]
pub(crate) struct ProxyRequestMeta {
    pub remote_ip: String,
    pub forwarded_for: String,
    pub host: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProxyTunnel {
    pub id: String,
    pub status: String,
    pub enabled: bool,
    pub access_policy: String,
    pub access_token_hash: Option<String>,
    pub access_username: Option<String>,
    pub access_password_hash: Option<String>,
    pub allowed_methods: Vec<String>,
    pub blocked_path_prefixes: Vec<String>,
    pub inspector_enabled: bool,
    pub rate_limit_per_minute: Option<i64>,
}

pub async fn root(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Response {
    let cfg = state.config.read().await.clone();
    if cfg.setup_required() {
        return Redirect::temporary("/admin/setup").into_response();
    }

    let host = request_host(&req);
    if cfg
        .domain
        .as_deref()
        .is_some_and(|domain| host == domain || host == format!("www.{domain}"))
    {
        root_status_page()
    } else {
        fallback_inner(state, req, remote_addr)
            .await
            .into_response()
    }
}

pub async fn admin(State(state): State<AppState>) -> impl IntoResponse {
    if state.config.read().await.setup_required() {
        Redirect::temporary("/admin/setup").into_response()
    } else {
        Html(pages::admin_html()).into_response()
    }
}

pub async fn setup_page() -> Html<&'static str> {
    Html(pages::setup_html())
}

pub async fn login_page() -> Html<&'static str> {
    Html(pages::login_html())
}

pub async fn static_asset(Path(path): Path<String>) -> Response {
    let (content_type, body) = match path.as_str() {
        "admin.html" => ("text/html; charset=utf-8", pages::admin_html()),
        "login.html" => ("text/html; charset=utf-8", pages::login_html()),
        "setup.html" => ("text/html; charset=utf-8", pages::setup_html()),
        "index.html" => ("text/html; charset=utf-8", pages::index_html()),
        _ => {
            return tunnel_error(
                StatusCode::NOT_FOUND,
                "asset_not_found",
                "static asset was not found",
            );
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            tunnel_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "asset_error",
                "failed to serve static asset",
            )
        })
}

pub async fn fallback(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Response {
    fallback_inner(state, req, remote_addr).await
}

async fn fallback_inner(state: AppState, req: Request<Body>, remote_addr: SocketAddr) -> Response {
    let cfg = state.config.read().await.clone();
    let host = request_host(&req);
    let meta = proxy_request_meta(req.headers(), remote_addr, &cfg);

    if cfg.setup_required() {
        return tunnel_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "setup_required",
            "setup is required before serving tunnels",
        );
    }

    let Some(domain) = cfg.domain.as_deref() else {
        return tunnel_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "setup_required",
            "domain is not configured",
        );
    };

    if host == domain || host == format!("www.{domain}") {
        return root_status_page();
    }

    let suffix = format!(".{domain}");
    let Some(subdomain) = host.strip_suffix(&suffix).map(normalize_subdomain) else {
        return tunnel_error(
            StatusCode::NOT_FOUND,
            "tunnel_not_found",
            "host is not served",
        );
    };

    let row = match sqlx::query(
        "SELECT id, status, enabled, access_policy, access_token_hash, access_username, access_password_hash, \
                allowed_methods, blocked_path_prefixes, inspector_enabled, rate_limit_per_minute \
         FROM tunnels WHERE subdomain = ?1 AND status != 'deleted'",
    )
    .bind(&subdomain)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(error) => {
            tracing::error!(%error, "failed to load tunnel for fallback proxy");
            return tunnel_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "failed to load tunnel",
            );
        }
    };

    let Some(row) = row else {
        return tunnel_error(
            StatusCode::NOT_FOUND,
            "tunnel_not_found",
            "tunnel was not found",
        );
    };

    let tunnel = row_to_proxy_tunnel(row);
    if tunnel.status == "expired" {
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            StatusCode::GONE,
            "tunnel_expired",
        )
        .await;
        return tunnel_error(StatusCode::GONE, "tunnel_expired", "tunnel has expired");
    }
    if !tunnel.enabled || tunnel.status == "disabled" {
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            StatusCode::FORBIDDEN,
            "tunnel_disabled",
        )
        .await;
        return tunnel_error(
            StatusCode::FORBIDDEN,
            "tunnel_disabled",
            "tunnel is disabled",
        );
    }

    if !method_allowed(req.method().as_str(), &tunnel.allowed_methods) {
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        )
        .await;
        return tunnel_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "method is not allowed for this tunnel",
        );
    }
    if path_blocked(req.uri().path(), &tunnel.blocked_path_prefixes) {
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            StatusCode::FORBIDDEN,
            "path_blocked",
        )
        .await;
        return tunnel_error(
            StatusCode::FORBIDDEN,
            "path_blocked",
            "path is blocked for this tunnel",
        );
    }
    if let Some(response) = enforce_tunnel_access(&tunnel, &req) {
        let reason = response
            .headers()
            .get("x-http-tunnel-reason")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("access_denied")
            .to_string();
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            response.status(),
            &reason,
        )
        .await;
        return response;
    }
    if let Some(response) = enforce_per_tunnel_rate_limit(&state, &cfg, &tunnel).await {
        let reason = response
            .headers()
            .get("x-http-tunnel-reason")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("tunnel_rate_limited")
            .to_string();
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            response.status(),
            &reason,
        )
        .await;
        return response;
    }

    let session = state
        .select_session(&subdomain, &tunnel.id, effective_session_pool_policy(&cfg))
        .await;
    let Some(session) = session else {
        let (method, path) = request_log_method_path(&req);
        log_blocked_request(
            &state,
            &tunnel,
            method,
            path,
            &meta,
            StatusCode::SERVICE_UNAVAILABLE,
            "tunnel_offline",
        )
        .await;
        return tunnel_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "tunnel_offline",
            "tunnel is offline",
        );
    };

    if is_ws_upgrade(&req) {
        return ws::forward_ws_to_tunnel(state, session, subdomain, req, meta).await;
    }

    http::forward_to_tunnel(
        state,
        session,
        subdomain,
        req,
        meta,
        tunnel.inspector_enabled,
    )
    .await
}

async fn log_blocked_request(
    state: &AppState,
    tunnel: &ProxyTunnel,
    method: String,
    path: String,
    meta: &ProxyRequestMeta,
    status: StatusCode,
    reason: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO request_logs (id, tunnel_id, request_type, method, path, host, remote_ip, user_agent, status, completed_at, duration_ms, bytes_in, bytes_out, error) \
         VALUES (?1, ?2, 'blocked', ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP, 0, 0, 0, ?9)",
    )
    .bind(generate_request_id())
    .bind(&tunnel.id)
    .bind(method)
    .bind(path)
    .bind(meta.host.as_deref())
    .bind(&meta.remote_ip)
    .bind(meta.user_agent.as_deref())
    .bind(i64::from(status.as_u16()))
    .bind(reason)
    .execute(&state.pool)
    .await;
}

fn request_log_method_path(req: &Request<Body>) -> (String, String) {
    (
        req.method().as_str().to_string(),
        req.uri()
            .path_and_query()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| "/".to_string()),
    )
}

fn row_to_proxy_tunnel(row: sqlx::sqlite::SqliteRow) -> ProxyTunnel {
    ProxyTunnel {
        id: row.get("id"),
        status: row.get("status"),
        enabled: row.get("enabled"),
        access_policy: row
            .try_get::<String, _>("access_policy")
            .unwrap_or_else(|_| "public".to_string()),
        access_token_hash: row
            .try_get::<Option<String>, _>("access_token_hash")
            .ok()
            .flatten(),
        access_username: row
            .try_get::<Option<String>, _>("access_username")
            .ok()
            .flatten(),
        access_password_hash: row
            .try_get::<Option<String>, _>("access_password_hash")
            .ok()
            .flatten(),
        allowed_methods: json_string_array(
            row.try_get::<Option<String>, _>("allowed_methods")
                .ok()
                .flatten()
                .as_deref(),
        ),
        blocked_path_prefixes: json_string_array(
            row.try_get::<Option<String>, _>("blocked_path_prefixes")
                .ok()
                .flatten()
                .as_deref(),
        ),
        inspector_enabled: row.try_get::<bool, _>("inspector_enabled").unwrap_or(false),
        rate_limit_per_minute: row
            .try_get::<Option<i64>, _>("rate_limit_per_minute")
            .ok()
            .flatten(),
    }
}

fn json_string_array(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
}

fn method_allowed(method: &str, allowed_methods: &[String]) -> bool {
    allowed_methods.is_empty()
        || allowed_methods
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(method))
}

fn path_blocked(path: &str, blocked_prefixes: &[String]) -> bool {
    blocked_prefixes
        .iter()
        .any(|prefix| path == prefix || path.starts_with(prefix))
}

fn enforce_tunnel_access(tunnel: &ProxyTunnel, req: &Request<Body>) -> Option<Response> {
    match tunnel.access_policy.as_str() {
        "public" => None,
        "bearer" => {
            let allowed = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .zip(tunnel.access_token_hash.as_deref())
                .is_some_and(|(token, hash)| verify_token(token, hash));
            if allowed {
                None
            } else {
                Some(tunnel_error(
                    StatusCode::UNAUTHORIZED,
                    "access_token_required",
                    "valid bearer access token is required",
                ))
            }
        }
        "basic" => {
            let allowed = basic_credentials(req.headers())
                .zip(
                    tunnel
                        .access_username
                        .as_deref()
                        .zip(tunnel.access_password_hash.as_deref()),
                )
                .is_some_and(
                    |((username, password), (expected_username, password_hash))| {
                        username == expected_username && verify_password(&password, password_hash)
                    },
                );
            if allowed {
                None
            } else {
                let mut response = tunnel_error(
                    StatusCode::UNAUTHORIZED,
                    "basic_auth_required",
                    "valid basic credentials are required",
                );
                response.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    header::HeaderValue::from_static("Basic realm=\"http-tunnel\""),
                );
                Some(response)
            }
        }
        _ => Some(tunnel_error(
            StatusCode::FORBIDDEN,
            "invalid_access_policy",
            "tunnel access policy is invalid",
        )),
    }
}

fn basic_credentials(headers: &axum::http::HeaderMap) -> Option<(String, String)> {
    let encoded = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Basic ")?;
    let decoded = BASE64_STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

async fn enforce_per_tunnel_rate_limit(
    state: &AppState,
    cfg: &ServerConfig,
    tunnel: &ProxyTunnel,
) -> Option<Response> {
    let limit = tunnel
        .rate_limit_per_minute
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(cfg.per_tunnel_rate_limit_per_minute);
    if limit == 0 {
        return None;
    }
    let now = std::time::Instant::now();
    let window = Duration::from_secs(60);
    let mut hits = state.per_tunnel_hits.write().await;
    let bucket: &mut VecDeque<std::time::Instant> = hits.entry(tunnel.id.clone()).or_default();
    while bucket
        .front()
        .is_some_and(|seen| now.duration_since(*seen) > window)
    {
        bucket.pop_front();
    }
    if bucket.len() >= limit as usize {
        return Some(tunnel_error(
            StatusCode::TOO_MANY_REQUESTS,
            "tunnel_rate_limited",
            "too many requests for this tunnel",
        ));
    }
    bucket.push_back(now);
    None
}

fn request_host(req: &Request<Body>) -> String {
    req.headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|host| host.split(':').next())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn root_status_page() -> Response {
    Html("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>http-tunnel</title><style>body{font-family:system-ui,sans-serif;margin:0;background:#f7f8fa;color:#171717}.wrap{max-width:900px;margin:12vh auto;padding:0 24px}h1{font-size:32px;margin:0 0 8px}.status{display:inline-block;margin-top:16px;padding:8px 12px;border:1px solid #c9d8c5;background:#edf8ea;color:#225516;border-radius:6px}</style></head><body><main class=\"wrap\"><h1>http-tunnel</h1><p>Server is running.</p><span class=\"status\">healthy</span></main></body></html>")
        .into_response()
}

pub(crate) fn tunnel_error(
    status: StatusCode,
    reason: &str,
    message: &str,
) -> axum::response::Response {
    let body: ApiResponse<()> = ApiResponse::err(reason, message);
    let mut response = (status, axum::Json(body)).into_response();
    response.headers_mut().insert(
        "x-http-tunnel-error",
        header::HeaderValue::from_static("true"),
    );
    if let Ok(value) = header::HeaderValue::from_str(reason) {
        response.headers_mut().insert("x-http-tunnel-reason", value);
    }
    response
}

pub(crate) async fn send_frame(
    session: &ActiveSession,
    frame: Frame,
) -> std::result::Result<(), String> {
    session
        .tx
        .send(frame)
        .await
        .map_err(|_| "tunnel is offline".to_string())
}

fn header_bytes(headers: &axum::http::HeaderMap) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
        .sum()
}

fn is_ws_upgrade(req: &Request<Body>) -> bool {
    req.headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && req
            .headers()
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
}

fn proxy_request_meta(
    headers: &axum::http::HeaderMap,
    remote_addr: SocketAddr,
    cfg: &ServerConfig,
) -> ProxyRequestMeta {
    let remote_ip = crate::net::client_ip(
        headers,
        remote_addr,
        cfg.trust_proxy_headers,
        &cfg.trusted_proxy_cidrs,
    );
    let forwarded_for = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            cfg.trust_proxy_headers
                && crate::net::proxy_is_trusted(remote_addr.ip(), &cfg.trusted_proxy_cidrs)
                && !value.trim().is_empty()
        })
        .map(|value| format!("{}, {}", value.trim(), remote_addr.ip()))
        .unwrap_or_else(|| remote_addr.ip().to_string());
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    ProxyRequestMeta {
        remote_ip,
        forwarded_for,
        host,
        user_agent,
    }
}

pub(crate) fn forwarded_header_allowed(name: &axum::http::HeaderName) -> bool {
    !matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "x-forwarded-for" | "x-forwarded-host" | "x-forwarded-proto" | "x-http-tunnel-subdomain"
    )
}
