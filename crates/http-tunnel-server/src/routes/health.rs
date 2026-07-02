use crate::state::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use http_tunnel_common::{
    api::ApiResponse, build_info::BuildInfo, country::normalize_country_code,
};
use serde::Serialize;
use sqlx::Row;
use std::{collections::HashMap, net::IpAddr, time::UNIX_EPOCH};

const MAX_PUBLIC_TUNNELS: i64 = 2_000;

#[derive(Debug, Serialize)]
pub struct Health {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Readiness {
    pub status: &'static str,
    pub setup_required: bool,
    pub database: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Version {
    pub version: &'static str,
    pub protocol_version: u8,
}

#[derive(Debug, Serialize)]
pub struct DashboardSummary {
    pub ready: &'static str,
    pub setup_required: bool,
    pub generated_at_unix_seconds: u64,
    pub server_url: Option<String>,
    pub stats: DashboardStats,
    pub tunnels: Vec<PublicTunnel>,
    pub country_sources: Vec<PublicTunnelCountrySource>,
}

#[derive(Debug, Serialize, Default)]
pub struct DashboardStats {
    pub total_tunnels: usize,
    pub online_tunnels: usize,
    pub offline_tunnels: usize,
    pub active_sessions: usize,
    pub active_streams: usize,
    pub request_count: i64,
    pub error_count: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub located_sources: usize,
    pub unknown_sources: usize,
}

#[derive(Debug, Serialize)]
pub struct PublicTunnel {
    pub subdomain: String,
    pub url: String,
    pub status: String,
    pub connected: bool,
    pub active_sessions: usize,
    pub active_streams: usize,
    pub request_count: i64,
    pub error_count: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub source: PublicTunnelSource,
    pub last_seen_at: Option<String>,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct PublicTunnelSource {
    pub label: String,
    pub country_code: Option<String>,
    pub country: Option<String>,
    pub located: bool,
}

#[derive(Debug, Serialize)]
pub struct PublicTunnelCountrySource {
    pub country_code: String,
    pub country: Option<String>,
    pub client_count: usize,
    pub tunnel_count: usize,
}

#[derive(Debug, Default)]
struct RuntimeTunnelMetrics {
    active_sessions: usize,
    active_streams: usize,
    session_ids: Vec<String>,
}

pub async fn health() -> Json<ApiResponse<Health>> {
    Json(ApiResponse::ok(Health { status: "ok" }))
}

pub async fn dashboard(State(state): State<AppState>) -> Json<ApiResponse<DashboardSummary>> {
    let cfg = state.config.read().await.clone();
    let setup_required = cfg.setup_required();
    let database_ok = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let runtime = runtime_tunnel_metrics(&state).await;
    let tunnels = public_tunnels(&state, &cfg, &runtime).await;
    let country_sources = public_country_sources(&state, &runtime).await;
    let stats = dashboard_stats(&tunnels, &country_sources);

    Json(ApiResponse::ok(DashboardSummary {
        ready: if setup_required || !database_ok {
            "not_ready"
        } else {
            "ready"
        },
        setup_required,
        generated_at_unix_seconds: unix_now(),
        server_url: dashboard_server_url(&cfg),
        stats,
        tunnels,
        country_sources,
    }))
}

fn dashboard_server_url(cfg: &http_tunnel_common::ServerConfig) -> Option<String> {
    let scheme = cfg.public_scheme.trim();
    let domain = cfg.domain.as_deref()?.trim();
    if scheme.is_empty() || domain.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{domain}"))
}

pub async fn ready(State(state): State<AppState>) -> axum::response::Response {
    let setup_required = state.config.read().await.setup_required();
    let database_ok = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let status = if setup_required || !database_ok {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let body = if database_ok {
        ApiResponse::ok(Readiness {
            status: if setup_required {
                "setup_required"
            } else {
                "ready"
            },
            setup_required,
            database: "ok",
        })
    } else {
        ApiResponse::<Readiness>::err("database_unavailable", "database readiness check failed")
    };
    (status, Json(body)).into_response()
}

pub async fn version() -> Json<ApiResponse<Version>> {
    Json(ApiResponse::ok(Version {
        version: env!("CARGO_PKG_VERSION"),
        protocol_version: http_tunnel_protocol::version::VERSION,
    }))
}

pub async fn version_full() -> Json<ApiResponse<BuildInfo>> {
    Json(ApiResponse::ok(BuildInfo::current()))
}

async fn runtime_tunnel_metrics(state: &AppState) -> HashMap<String, RuntimeTunnelMetrics> {
    let sessions = state.sessions_by_subdomain.read().await;
    let mut metrics = HashMap::<String, RuntimeTunnelMetrics>::new();
    for session in sessions.values().flat_map(|pool| pool.sessions.iter()) {
        if session.tx.is_closed() {
            continue;
        }
        let snapshot = session.runtime_metrics();
        let entry = metrics.entry(session.tunnel_id.clone()).or_default();
        entry.active_sessions += 1;
        entry.active_streams += snapshot.active_streams;
        entry.session_ids.push(session.session_id.clone());
    }
    metrics
}

async fn public_tunnels(
    state: &AppState,
    cfg: &http_tunnel_common::ServerConfig,
    runtime: &HashMap<String, RuntimeTunnelMetrics>,
) -> Vec<PublicTunnel> {
    let rows = sqlx::query(
        "SELECT t.id, t.subdomain, t.status, t.enabled, t.expires_at, t.client_ip, \
                ls.remote_addr AS latest_remote_addr, ls.client_reported_ip AS latest_reported_ip, \
                ls.client_country_code AS latest_country_code, \
                ls.client_country AS latest_country, ls.last_seen_at AS latest_seen_at, \
                COALESCE(req.request_count, 0) AS request_count, \
                COALESCE(req.error_count, 0) AS error_count, \
                COALESCE(req.bytes_in, 0) AS bytes_in, \
                COALESCE(req.bytes_out, 0) AS bytes_out \
         FROM tunnels t \
         LEFT JOIN sessions ls ON ls.id = ( \
             SELECT id FROM sessions WHERE tunnel_id = t.id ORDER BY connected_at DESC LIMIT 1 \
         ) \
         LEFT JOIN ( \
             SELECT tunnel_id, COUNT(*) AS request_count, \
                    SUM(CASE WHEN error IS NOT NULL THEN 1 ELSE 0 END) AS error_count, \
                    SUM(COALESCE(bytes_in, 0)) AS bytes_in, \
                    SUM(COALESCE(bytes_out, 0)) AS bytes_out \
             FROM request_logs GROUP BY tunnel_id \
         ) req ON req.tunnel_id = t.id \
         WHERE t.status != 'deleted' \
         ORDER BY CASE WHEN t.status = 'connected' THEN 0 \
                       WHEN t.status = 'reserved' THEN 1 \
                       WHEN t.status = 'disconnected' THEN 2 \
                       ELSE 3 END, t.subdomain ASC \
         LIMIT ?1",
    )
    .bind(MAX_PUBLIC_TUNNELS)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| {
            let id = row.get::<String, _>("id");
            let subdomain = row.get::<String, _>("subdomain");
            let status = row.get::<String, _>("status");
            let runtime_metrics = runtime.get(&id);
            let active_sessions = runtime_metrics
                .map(|metrics| metrics.active_sessions)
                .unwrap_or_default();
            let active_streams = runtime_metrics
                .map(|metrics| metrics.active_streams)
                .unwrap_or_default();
            let connected = active_sessions > 0;
            let source_ip = source_ip(
                row.try_get::<Option<String>, _>("latest_reported_ip")
                    .ok()
                    .flatten()
                    .as_deref(),
                row.try_get::<Option<String>, _>("latest_remote_addr")
                    .ok()
                    .flatten()
                    .as_deref(),
                row.try_get::<Option<String>, _>("client_ip")
                    .ok()
                    .flatten()
                    .as_deref(),
            );
            let source_present = source_ip.is_some();
            let row_country_code = row
                .try_get::<Option<String>, _>("latest_country_code")
                .ok()
                .flatten()
                .and_then(|code| normalize_country_code(&code));
            let row_country = row
                .try_get::<Option<String>, _>("latest_country")
                .ok()
                .flatten()
                .filter(|value| !value.trim().is_empty());
            let source = public_source(row_country_code, row_country, source_present);
            PublicTunnel {
                url: cfg
                    .public_url(&subdomain)
                    .unwrap_or_else(|| format!("/{subdomain}")),
                subdomain,
                status,
                connected,
                active_sessions,
                active_streams,
                request_count: row.get::<i64, _>("request_count"),
                error_count: row.get::<i64, _>("error_count"),
                bytes_in: non_negative_u64(row.get::<i64, _>("bytes_in")),
                bytes_out: non_negative_u64(row.get::<i64, _>("bytes_out")),
                source,
                last_seen_at: row
                    .try_get::<Option<String>, _>("latest_seen_at")
                    .ok()
                    .flatten(),
                expires_at: row.get::<String, _>("expires_at"),
            }
        })
        .collect()
}

async fn public_country_sources(
    state: &AppState,
    runtime: &HashMap<String, RuntimeTunnelMetrics>,
) -> Vec<PublicTunnelCountrySource> {
    let session_ids = runtime
        .values()
        .flat_map(|metrics| metrics.session_ids.iter().cloned())
        .collect::<Vec<_>>();
    if session_ids.is_empty() {
        return Vec::new();
    }

    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT tunnel_id, client_country_code, client_country \
         FROM sessions WHERE id IN (",
    );
    let mut separated = builder.separated(", ");
    for session_id in &session_ids {
        separated.push_bind(session_id);
    }
    separated.push_unseparated(")");

    let mut by_country =
        HashMap::<String, (Option<String>, usize, std::collections::HashSet<String>)>::new();
    for row in builder
        .build()
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default()
    {
        let row_country_code = row
            .try_get::<Option<String>, _>("client_country_code")
            .ok()
            .flatten()
            .and_then(|code| normalize_country_code(&code));
        let row_country = row
            .try_get::<Option<String>, _>("client_country")
            .ok()
            .flatten()
            .filter(|value| !value.trim().is_empty());
        let Some(country_code) = row_country_code else {
            continue;
        };
        let country = row_country;
        let tunnel_id = row.get::<String, _>("tunnel_id");
        let entry = by_country
            .entry(country_code)
            .or_insert_with(|| (country.clone(), 0, std::collections::HashSet::new()));
        if entry.0.is_none() {
            entry.0 = country;
        }
        entry.1 += 1;
        entry.2.insert(tunnel_id);
    }

    let mut sources = by_country
        .into_iter()
        .map(
            |(country_code, (country, client_count, tunnel_ids))| PublicTunnelCountrySource {
                country_code,
                country,
                client_count,
                tunnel_count: tunnel_ids.len(),
            },
        )
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        right
            .client_count
            .cmp(&left.client_count)
            .then_with(|| left.country_code.cmp(&right.country_code))
    });
    sources
}

fn dashboard_stats(
    tunnels: &[PublicTunnel],
    country_sources: &[PublicTunnelCountrySource],
) -> DashboardStats {
    let located_sources = country_sources
        .iter()
        .map(|source| source.client_count)
        .sum::<usize>();
    let mut stats = DashboardStats {
        total_tunnels: tunnels.len(),
        located_sources,
        ..DashboardStats::default()
    };
    for tunnel in tunnels {
        if tunnel.connected {
            stats.online_tunnels += 1;
        } else {
            stats.offline_tunnels += 1;
        }
        stats.active_sessions += tunnel.active_sessions;
        stats.active_streams += tunnel.active_streams;
        stats.request_count += tunnel.request_count;
        stats.error_count += tunnel.error_count;
        stats.bytes_in = stats.bytes_in.saturating_add(tunnel.bytes_in);
        stats.bytes_out = stats.bytes_out.saturating_add(tunnel.bytes_out);
    }
    stats.unknown_sources = stats.active_sessions.saturating_sub(stats.located_sources);
    stats
}

fn source_ip(
    latest_reported_ip: Option<&str>,
    latest_remote_addr: Option<&str>,
    client_ip: Option<&str>,
) -> Option<IpAddr> {
    latest_reported_ip
        .and_then(parse_ip)
        .or_else(|| latest_remote_addr.and_then(parse_ip))
        .or_else(|| client_ip.and_then(parse_ip))
}

fn parse_ip(raw: &str) -> Option<IpAddr> {
    let value = raw.split(',').next()?.trim();
    if value.is_empty() {
        return None;
    }
    value
        .parse::<std::net::SocketAddr>()
        .map(|addr| addr.ip())
        .ok()
        .or_else(|| value.trim_matches(['[', ']']).parse::<IpAddr>().ok())
}

fn public_source(
    country_code: Option<String>,
    country: Option<String>,
    source_present: bool,
) -> PublicTunnelSource {
    let Some(country_code) = country_code else {
        if let Some(country) = country.filter(|value| !value.trim().is_empty()) {
            return PublicTunnelSource {
                label: country.clone(),
                country_code: None,
                country: Some(country),
                located: false,
            };
        }
        return PublicTunnelSource {
            label: if source_present {
                "Unknown country".to_string()
            } else {
                "Unknown source".to_string()
            },
            country_code: None,
            country: None,
            located: false,
        };
    };
    let label = country
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&country_code)
        .to_string();
    PublicTunnelSource {
        label,
        country_code: Some(country_code),
        country,
        located: true,
    }
}

fn non_negative_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or_default()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
