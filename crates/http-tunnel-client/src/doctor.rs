use crate::{
    config::{default_config_path, ClientConfig},
    connect::tunnel_ws_url,
};
use http_tunnel_common::api::ApiResponse;
use http_tunnel_protocol::version::VERSION as PROTOCOL_VERSION;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

pub async fn run_doctor(
    server: Option<String>,
    target: Option<String>,
    subdomain: Option<String>,
    cfg: ClientConfig,
    json: bool,
    websocket_path: Option<String>,
) -> anyhow::Result<()> {
    let server = server.or(cfg.server.clone());
    let target = target.or(cfg.target.clone());
    let subdomain = subdomain.or(cfg.subdomain.clone());
    let report = build_report(server, target, subdomain, &cfg, websocket_path).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    if report.ok {
        Ok(())
    } else {
        anyhow::bail!("doctor found errors")
    }
}

async fn build_report(
    server: Option<String>,
    target: Option<String>,
    subdomain: Option<String>,
    cfg: &ClientConfig,
    websocket_path: Option<String>,
) -> DoctorReport {
    let mut checks = Vec::new();
    checks.push(ok(
        "config",
        format!("loaded {}", default_config_path().display()),
    ));

    match server.as_deref() {
        Some(server) if valid_server_url(server) => {
            checks.push(ok("server_config", format!("server is {server}")));
            checks.push(check_server_health(server).await);
            checks.push(check_server_protocol(server).await);
        }
        Some(server) => checks.push(error(
            "server_config",
            format!("server must start with http:// or https://: {server}"),
        )),
        None => checks.push(error(
            "server_config",
            "server is required via --server or client config",
        )),
    }

    match target.as_deref() {
        Some(target) if valid_target_url(target) => {
            checks.push(ok("target_config", format!("target is {target}")));
            checks.push(check_target(target).await);
            if let Some(path) = websocket_path.as_deref() {
                checks.push(check_websocket_target(target, path).await);
            }
        }
        Some(target) => checks.push(error(
            "target_config",
            format!("target must start with http:// or https://: {target}"),
        )),
        None => checks.push(error(
            "target_config",
            "target is required via --target or client config",
        )),
    }

    if let Some(subdomain) = subdomain {
        checks.push(ok("subdomain", format!("subdomain is {subdomain}")));
    } else {
        checks.push(warn(
            "subdomain",
            "no subdomain configured; server will assign a random one",
        ));
    }

    match (
        server.as_deref(),
        cfg.tunnel_id.as_deref(),
        cfg.token.as_deref(),
    ) {
        (Some(server), Some(tunnel_id), Some(token)) if valid_server_url(server) => {
            match tunnel_ws_url(server, tunnel_id, token) {
                Ok(url) => checks.push(ok("stored_tunnel", format!("websocket URL: {url}"))),
                Err(err) => checks.push(error("stored_tunnel", err.to_string())),
            }
            checks.push(check_stored_tunnel_state(server, tunnel_id).await);
        }
        (_, Some(_), None) | (_, None, Some(_)) => checks.push(error(
            "stored_tunnel",
            "stored tunnel credentials are incomplete; clear or set both tunnel_id and token",
        )),
        _ => checks.push(warn(
            "stored_tunnel",
            "no stored tunnel token; a new tunnel will be created",
        )),
    }

    let ok = !checks
        .iter()
        .any(|check| check.status == DoctorStatus::Error);
    DoctorReport { ok, checks }
}

async fn check_server_health(server: &str) -> DoctorCheck {
    let client = http_client();
    let url = format!("{}/api/v1/health", server.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => ok(
            "server_health",
            format!("{url} returned {}", response.status()),
        ),
        Ok(response) => error(
            "server_health",
            format!("{url} returned {}", response.status()),
        ),
        Err(err) => error("server_health", format!("failed to reach {url}: {err}")),
    }
}

async fn check_server_protocol(server: &str) -> DoctorCheck {
    let client = http_client();
    let url = format!("{}/api/v1/version", server.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<ApiResponse<serde_json::Value>>().await {
                Ok(value) => {
                    let protocol_version = value
                        .data
                        .and_then(|data| data["protocol_version"].as_u64())
                        .unwrap_or_default();
                    if protocol_version == u64::from(PROTOCOL_VERSION) {
                        ok(
                            "server_protocol",
                            format!("protocol version {protocol_version}"),
                        )
                    } else {
                        warn(
                            "server_protocol",
                            format!(
                                "server protocol version {protocol_version}, client expects {PROTOCOL_VERSION}"
                            ),
                        )
                    }
                }
                Err(err) => warn(
                    "server_protocol",
                    format!("failed to decode version: {err}"),
                ),
            }
        }
        Ok(response) => warn(
            "server_protocol",
            format!("{url} returned {}", response.status()),
        ),
        Err(err) => warn("server_protocol", format!("failed to reach {url}: {err}")),
    }
}

async fn check_target(target: &str) -> DoctorCheck {
    let client = http_client();
    match client.get(target).send().await {
        Ok(response) => ok(
            "target_reachable",
            format!("target returned {}", response.status()),
        ),
        Err(err) => error("target_reachable", format!("failed to reach target: {err}")),
    }
}

async fn check_websocket_target(target: &str, path: &str) -> DoctorCheck {
    let url = match websocket_target_url(target, path) {
        Ok(url) => url,
        Err(err) => return error("target_websocket", err.to_string()),
    };
    match tokio_tungstenite::connect_async(&url).await {
        Ok((mut ws, _)) => {
            let _ = ws.close(None).await;
            ok("target_websocket", format!("websocket accepted {url}"))
        }
        Err(err) => error(
            "target_websocket",
            format!("failed to connect {url}: {err}"),
        ),
    }
}

async fn check_stored_tunnel_state(server: &str, tunnel_id: &str) -> DoctorCheck {
    let client = http_client();
    let url = format!(
        "{}/api/v1/tunnels/{tunnel_id}",
        server.trim_end_matches('/')
    );
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<ApiResponse<serde_json::Value>>().await {
                Ok(value) => {
                    let status = value
                        .data
                        .and_then(|data| data["status"].as_str().map(ToString::to_string))
                        .unwrap_or_else(|| "unknown".to_string());
                    if matches!(status.as_str(), "expired" | "deleted" | "disabled") {
                        error("stored_tunnel_state", format!("stored tunnel is {status}"))
                    } else {
                        ok("stored_tunnel_state", format!("stored tunnel is {status}"))
                    }
                }
                Err(err) => error(
                    "stored_tunnel_state",
                    format!("failed to decode tunnel: {err}"),
                ),
            }
        }
        Ok(response) => error(
            "stored_tunnel_state",
            format!("{url} returned {}", response.status()),
        ),
        Err(err) => error(
            "stored_tunnel_state",
            format!("failed to reach {url}: {err}"),
        ),
    }
}

fn websocket_target_url(target: &str, path: &str) -> anyhow::Result<String> {
    let scheme = if target.starts_with("https://") {
        "wss://"
    } else if target.starts_with("http://") {
        "ws://"
    } else {
        anyhow::bail!("target must start with http:// or https://");
    };
    let rest = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
        .expect("validated prefix");
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Ok(format!("{scheme}{}{path}", rest.trim_end_matches('/')))
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn valid_server_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn valid_target_url(value: &str) -> bool {
    valid_server_url(value)
}

fn ok(name: &str, message: impl Into<String>) -> DoctorCheck {
    check(name, DoctorStatus::Ok, message)
}

fn warn(name: &str, message: impl Into<String>) -> DoctorCheck {
    check(name, DoctorStatus::Warn, message)
}

fn error(name: &str, message: impl Into<String>) -> DoctorCheck {
    check(name, DoctorStatus::Error, message)
}

fn check(name: &str, status: DoctorStatus, message: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status,
        message: message.into(),
    }
}

fn print_report(report: &DoctorReport) {
    for check in &report.checks {
        let status = match check.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warn => "warn",
            DoctorStatus::Error => "error",
        };
        println!("{status:5} {} - {}", check.name, check.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_missing_required_config_as_errors() {
        let report = build_report(None, None, None, &ClientConfig::default(), None).await;
        assert!(!report.ok);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "server_config" && check.status == DoctorStatus::Error));
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "target_config" && check.status == DoctorStatus::Error));
    }

    #[tokio::test]
    async fn reports_partial_stored_tunnel_as_error() {
        let cfg = ClientConfig {
            server: Some("http://127.0.0.1:8080".to_string()),
            target: Some("http://127.0.0.1:3000".to_string()),
            tunnel_id: Some("tun_123".to_string()),
            ..ClientConfig::default()
        };
        let report = build_report(cfg.server.clone(), cfg.target.clone(), None, &cfg, None).await;
        assert!(!report.ok);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "stored_tunnel" && check.status == DoctorStatus::Error));
    }
}
