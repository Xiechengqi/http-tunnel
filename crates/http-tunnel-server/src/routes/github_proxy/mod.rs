use crate::state::AppState;
use axum::{
    body::Body,
    extract::{connect_info::ConnectInfo, Path, Query, State},
    http::{header, Request, StatusCode},
    response::Response,
};
use http_tunnel_common::ServerConfig;
use std::{collections::HashMap, net::SocketAddr, time::Duration};

mod matcher;
mod rewrite;
mod rules;
mod upstream;

pub(crate) fn route_prefix(config: &ServerConfig) -> String {
    let prefix = config.github_proxy_server_path_prefix.trim();
    if valid_route_prefix(prefix) {
        prefix.to_string()
    } else {
        tracing::warn!(
            configured_prefix = prefix,
            "invalid github proxy server path prefix; using /gh"
        );
        "/gh".to_string()
    }
}

pub async fn entry(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Query(params): Query<HashMap<String, String>>,
    req: Request<Body>,
) -> Response {
    let cfg = state.config.read().await.clone();
    if !root_domain_request(req.headers(), &cfg) {
        return super::proxy::fallback(State(state), ConnectInfo(remote_addr), req).await;
    }
    if !cfg.github_proxy_server_enabled {
        return plain_response(StatusCode::NOT_FOUND, "github proxy server is disabled");
    }
    let route_prefix = state.github_proxy_route_prefix.clone();
    if let Some(target) = params
        .get("q")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    {
        return redirect_response(StatusCode::FOUND, &format!("{route_prefix}/{target}"));
    }
    plain_response(StatusCode::OK, "github proxy server is enabled")
}

pub async fn proxy(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(target): Path<String>,
    req: Request<Body>,
) -> Response {
    let cfg = state.config.read().await.clone();
    if !root_domain_request(req.headers(), &cfg) {
        return super::proxy::fallback(State(state), ConnectInfo(remote_addr), req).await;
    }
    if !cfg.github_proxy_server_enabled {
        return plain_response(StatusCode::NOT_FOUND, "github proxy server is disabled");
    }

    let query = req.uri().query().map(ToString::to_string);
    let route_prefix = state.github_proxy_route_prefix.clone();
    let mut upstream_target = matcher::normalize_input(&target, query.as_deref());
    let Some((_, matched)) = matcher::parse_allowed_url(&upstream_target) else {
        return plain_response(StatusCode::FORBIDDEN, "Invalid input.");
    };

    let decision = match rules::evaluate(
        &matched,
        &cfg.github_proxy_server_white_list,
        &cfg.github_proxy_server_black_list,
        &cfg.github_proxy_server_pass_list,
    ) {
        Ok(decision) => decision,
        Err(rules::AccessDenied::WhiteList) => {
            return plain_response(StatusCode::FORBIDDEN, "Forbidden by white list.");
        }
        Err(rules::AccessDenied::BlackList) => {
            return plain_response(StatusCode::FORBIDDEN, "Forbidden by black list.");
        }
    };

    if (cfg.github_proxy_server_jsdelivr || decision == rules::AccessDecision::PassBy)
        && matches!(
            matched.kind,
            matcher::GithubUrlKind::BlobOrRaw | matcher::GithubUrlKind::RawFile
        )
    {
        if let Some(url) = rewrite::jsdelivr_url(&upstream_target) {
            return redirect_response(StatusCode::FOUND, &url);
        }
    }

    if decision == rules::AccessDecision::PassBy {
        return redirect_response(StatusCode::FOUND, &upstream_target);
    }

    if matched.kind == matcher::GithubUrlKind::BlobOrRaw {
        upstream_target = rewrite::blob_to_raw(&upstream_target);
    }
    let Some((upstream_url, _)) = matcher::parse_allowed_url(&upstream_target) else {
        return plain_response(StatusCode::FORBIDDEN, "Invalid input.");
    };

    upstream::proxy_request(
        &state.github_proxy_client,
        req,
        upstream_url,
        &route_prefix,
        cfg.github_proxy_server_size_limit_bytes,
        Duration::from_secs(cfg.github_proxy_server_request_timeout_seconds.max(1)),
        Some(state.start_github_proxy_activity()),
    )
    .await
}

pub(crate) fn plain_response(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) fn redirect_response(status: StatusCode, location: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .unwrap_or_else(|_| {
            plain_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to build redirect response",
            )
        })
}

fn valid_route_prefix(value: &str) -> bool {
    !value.is_empty()
        && value != "/"
        && value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("//")
        && !value.contains('*')
        && !value.contains(':')
        && !value.contains('?')
        && !value.contains('#')
        && !value.split('/').any(|part| part == "." || part == "..")
        && !reserved_path_prefix(value)
}

fn reserved_path_prefix(value: &str) -> bool {
    [
        "/admin",
        "/api",
        "/assets",
        "/_next",
        "/metrics",
        "/icon.png",
        "/icon.svg",
    ]
    .iter()
    .any(|reserved| value == *reserved || value.starts_with(&format!("{reserved}/")))
}

fn root_domain_request(headers: &axum::http::HeaderMap, cfg: &ServerConfig) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|host| host.split(':').next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(domain) = cfg.domain.as_deref() else {
        return true;
    };
    host == domain || host == format!("www.{domain}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_default_route_prefix_when_configured_prefix_is_invalid() {
        let mut cfg = ServerConfig {
            github_proxy_server_path_prefix: "/api".to_string(),
            ..ServerConfig::default()
        };
        assert_eq!(route_prefix(&cfg), "/gh");

        cfg.github_proxy_server_path_prefix = "/api/github".to_string();
        assert_eq!(route_prefix(&cfg), "/gh");

        cfg.github_proxy_server_path_prefix = "/github".to_string();
        assert_eq!(route_prefix(&cfg), "/github");
    }
}
