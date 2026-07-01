use crate::{geoip, state::AppState};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use http_tunnel_common::{api::ApiResponse, build_info::BuildInfo};
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
    pub map_points: Vec<PublicTunnelMapPoint>,
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
    pub region: Option<String>,
    pub city: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub located: bool,
}

#[derive(Debug, Serialize)]
pub struct PublicTunnelMapPoint {
    pub subdomain: String,
    pub status: String,
    pub label: String,
    pub latitude: f64,
    pub longitude: f64,
    pub active_sessions: usize,
}

#[derive(Debug, Default)]
struct RuntimeTunnelMetrics {
    active_sessions: usize,
    active_streams: usize,
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
    let map_points = tunnels
        .iter()
        .filter_map(|tunnel| {
            Some(PublicTunnelMapPoint {
                subdomain: tunnel.subdomain.clone(),
                status: tunnel.status.clone(),
                label: tunnel.source.label.clone(),
                latitude: tunnel.source.latitude?,
                longitude: tunnel.source.longitude?,
                active_sessions: tunnel.active_sessions,
            })
        })
        .collect::<Vec<_>>();
    let stats = dashboard_stats(&tunnels, &map_points);

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
        map_points,
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
                ls.remote_addr AS latest_remote_addr, ls.last_seen_at AS latest_seen_at, \
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
            let source = public_source(
                source_ip.and_then(|ip| geoip::lookup(&cfg.data_dir, ip)),
                source_present,
            );
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

fn dashboard_stats(
    tunnels: &[PublicTunnel],
    map_points: &[PublicTunnelMapPoint],
) -> DashboardStats {
    let mut stats = DashboardStats {
        total_tunnels: tunnels.len(),
        located_sources: map_points.len(),
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
    stats
}

fn source_ip(latest_remote_addr: Option<&str>, client_ip: Option<&str>) -> Option<IpAddr> {
    latest_remote_addr
        .and_then(parse_ip)
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

fn public_source(location: Option<geoip::GeoLocation>, source_present: bool) -> PublicTunnelSource {
    let Some(location) = location else {
        return PublicTunnelSource {
            label: if source_present {
                "Unlocated source".to_string()
            } else {
                "Unknown source".to_string()
            },
            country_code: None,
            country: None,
            region: None,
            city: None,
            latitude: None,
            longitude: None,
            located: false,
        };
    };
    let label = location_label(&location);
    PublicTunnelSource {
        label,
        country_code: location.country_code,
        country: location.country,
        region: location.region,
        city: location.city,
        latitude: Some(location.latitude),
        longitude: Some(location.longitude),
        located: true,
    }
}

fn location_label(location: &geoip::GeoLocation) -> String {
    let mut parts = Vec::new();
    if let Some(city) = location.city.as_deref() {
        parts.push(city);
    }
    if let Some(region) = location.region.as_deref() {
        if !parts.iter().any(|part| *part == region) {
            parts.push(region);
        }
    }
    if let Some(country) = location.country.as_deref() {
        if !parts.iter().any(|part| *part == country) {
            parts.push(country);
        }
    } else if let Some(code) = location.country_code.as_deref() {
        parts.push(code);
    }
    if parts.is_empty() {
        "Located source".to_string()
    } else {
        parts.join(", ")
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
