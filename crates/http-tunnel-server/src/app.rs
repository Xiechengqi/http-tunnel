use crate::{db, routes, routes::startup_binary_sha256, state::AppState};
use anyhow::Context;
use axum::{
    body::Body,
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Request},
    middleware::{self, Next},
    response::Response,
    Router,
};
use http_tunnel_common::{ids::generate_event_id, ServerConfig};
use http_tunnel_protocol::{Frame, FrameType};
use serde::Serialize;
use sqlx::Row;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

pub(crate) const DISCONNECTED_TUNNEL_EXPIRE_AFTER: &str = "-10 minutes";
pub(crate) const EXPIRED_TUNNEL_DELETE_AFTER: &str = "-1 hour";
pub(crate) const SUBDOMAIN_CLAIM_AFTER_DISCONNECT: &str = "+1 hour";

pub async fn serve(config_path: String, config: ServerConfig) -> anyhow::Result<()> {
    let addr = config.addr;
    let pool = db::connect(&config.database_url).await?;
    let binary_sha256 = startup_binary_sha256().context("compute startup binary sha256")?;
    let state = AppState::new(config_path, config, pool, Some(binary_sha256));
    state
        .initialize_tunnel_traffic_from_request_logs()
        .await
        .context("initialize tunnel traffic counters")?;
    spawn_cleanup_job(state.clone());
    routes::spawn_auto_upgrade_job(state.clone());

    let app = Router::new()
        .merge(routes::router())
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(add_security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(%addr, "http-tunnel-server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("serve http")
}

async fn add_security_headers(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:",
        ),
    );
    if path == "/admin" || path.starts_with("/admin/") || path.starts_with("/api/admin/") {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        );
    }
    response
}

fn spawn_cleanup_job(state: AppState) {
    tokio::spawn(async move {
        loop {
            let interval_seconds = state.config.read().await.cleanup_interval_seconds.max(1);
            tokio::time::sleep(std::time::Duration::from_secs(interval_seconds)).await;
            if let Err(error) = cleanup_once(&state).await {
                tracing::warn!(%error, "background cleanup failed");
            }
        }
    });
}

#[derive(Debug, Serialize)]
pub(crate) struct CleanupSummary {
    pub deleted_client_ttl_tunnels: u64,
    pub expired_reserved: u64,
    pub expired_disconnected: u64,
    pub expired_connected: u64,
    pub soft_deleted_expired_tunnels: u64,
    pub stale_sessions: u64,
    pub deleted_request_logs: u64,
    pub deleted_events: u64,
    pub deleted_audit_logs: u64,
    pub deleted_sessions: u64,
}

pub(crate) async fn cleanup_once(state: &AppState) -> anyhow::Result<CleanupSummary> {
    let cfg = state.config.read().await.clone();
    let expired_client_ttl_ids = sqlx::query(
        "SELECT id FROM tunnels \
         WHERE status != 'deleted' AND client_ttl_seconds IS NOT NULL AND expires_at <= CURRENT_TIMESTAMP",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| row.get::<String, _>("id"))
    .collect::<Vec<_>>();
    let mut deleted_client_ttl_tunnels = 0;
    for tunnel_id in expired_client_ttl_ids {
        if delete_expired_client_ttl_tunnel(state, &tunnel_id).await? {
            deleted_client_ttl_tunnels += 1;
        }
    }
    let expired_reserved = sqlx::query(
        "UPDATE tunnels SET status = 'expired' \
         WHERE status = 'reserved' AND expires_at <= CURRENT_TIMESTAMP",
    )
    .execute(&state.pool)
    .await?
    .rows_affected();
    let expired_disconnected = sqlx::query(
        "UPDATE tunnels SET status = 'expired', expires_at = CURRENT_TIMESTAMP \
         WHERE status = 'disconnected' \
         AND COALESCE(disconnected_at, expires_at) <= datetime('now', ?1)",
    )
    .bind(DISCONNECTED_TUNNEL_EXPIRE_AFTER)
    .execute(&state.pool)
    .await?
    .rows_affected();
    let mut active_tunnel_ids = state.active_tunnel_ids().await;
    active_tunnel_ids.sort();
    active_tunnel_ids.dedup();
    let expired_connected = if active_tunnel_ids.is_empty() {
        sqlx::query(
            "UPDATE tunnels SET status = 'expired' \
             WHERE status = 'connected' AND expires_at <= CURRENT_TIMESTAMP",
        )
        .execute(&state.pool)
        .await?
        .rows_affected()
    } else {
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "UPDATE tunnels SET status = 'expired' \
             WHERE status = 'connected' AND expires_at <= CURRENT_TIMESTAMP AND id NOT IN (",
        );
        let mut separated = query.separated(", ");
        for id in active_tunnel_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        query.build().execute(&state.pool).await?.rows_affected()
    };
    let soft_deleted_expired_tunnels = sqlx::query(
        "UPDATE tunnels SET status = 'deleted' \
         WHERE status = 'expired' \
         AND (claim_expires_at <= CURRENT_TIMESTAMP \
              OR COALESCE(disconnected_at, expires_at) <= datetime('now', ?1))",
    )
    .bind(EXPIRED_TUNNEL_DELETE_AFTER)
    .execute(&state.pool)
    .await?
    .rows_affected();
    let stale_sessions = cleanup_stale_runtime_sessions(state, &cfg).await?;
    let deleted_request_logs = sqlx::query(
        "DELETE FROM request_logs \
         WHERE started_at < datetime('now', ?1)",
    )
    .bind(format!("-{} days", cfg.request_log_retention_days))
    .execute(&state.pool)
    .await?
    .rows_affected();
    let deleted_events = sqlx::query(
        "DELETE FROM events \
         WHERE created_at < datetime('now', ?1)",
    )
    .bind(format!("-{} days", cfg.event_retention_days))
    .execute(&state.pool)
    .await?
    .rows_affected();
    let deleted_audit_logs = sqlx::query(
        "DELETE FROM audit_logs \
         WHERE created_at < datetime('now', ?1)",
    )
    .bind(format!("-{} days", cfg.event_retention_days))
    .execute(&state.pool)
    .await?
    .rows_affected();
    let deleted_sessions = sqlx::query(
        "DELETE FROM sessions \
         WHERE disconnected_at IS NOT NULL \
         AND disconnected_at < datetime('now', ?1)",
    )
    .bind(format!("-{} days", cfg.session_retention_days))
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(CleanupSummary {
        deleted_client_ttl_tunnels,
        expired_reserved,
        expired_disconnected,
        expired_connected,
        soft_deleted_expired_tunnels,
        stale_sessions,
        deleted_request_logs,
        deleted_events,
        deleted_audit_logs,
        deleted_sessions,
    })
}

pub(crate) fn schedule_client_ttl_expiry(state: AppState, tunnel_id: String, ttl_seconds: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(ttl_seconds.max(1))).await;
        match delete_expired_client_ttl_tunnel(&state, &tunnel_id).await {
            Ok(true) => {
                tracing::info!(%tunnel_id, "client ttl expired; tunnel deleted");
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, %tunnel_id, "failed to delete expired client ttl tunnel");
            }
        }
    });
}

pub(crate) async fn delete_expired_client_ttl_tunnel(
    state: &AppState,
    tunnel_id: &str,
) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "UPDATE tunnels SET status = 'deleted', expires_at = CURRENT_TIMESTAMP, disconnected_at = CURRENT_TIMESTAMP, \
         claim_expires_at = CURRENT_TIMESTAMP \
         WHERE id = ?1 AND status != 'deleted' AND client_ttl_seconds IS NOT NULL AND expires_at <= CURRENT_TIMESTAMP",
    )
    .bind(tunnel_id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }

    let sessions = state.remove_sessions_for_tunnel(tunnel_id).await;
    for session in sessions {
        cancel_pending_streams_for_session(state, &session, "tunnel_expired").await;
        let _ = session
            .tx
            .send(Frame::new(FrameType::Goaway, 0, b"tunnel_expired".to_vec()))
            .await;
        sqlx::query(
            "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = 'tunnel_expired' \
             WHERE id = ?1 AND disconnected_at IS NULL",
        )
        .bind(&session.session_id)
        .execute(&state.pool)
        .await?;
    }

    sqlx::query(
        "INSERT INTO events (id, tunnel_id, kind, message) VALUES (?1, ?2, 'tunnel_deleted', 'client ttl expired')",
    )
    .bind(generate_event_id())
    .bind(tunnel_id)
    .execute(&state.pool)
    .await?;
    Ok(true)
}

async fn cleanup_stale_runtime_sessions(
    state: &AppState,
    cfg: &ServerConfig,
) -> anyhow::Result<u64> {
    let stale_after = std::time::Duration::from_secs(cfg.stale_session_seconds.max(1));
    let now = std::time::Instant::now();
    let sessions = state
        .sessions_by_subdomain
        .read()
        .await
        .iter()
        .flat_map(|(subdomain, pool)| {
            pool.sessions
                .iter()
                .map(|session| (subdomain.clone(), session.clone()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut stale = Vec::new();
    for (subdomain, session) in sessions {
        let last_seen = *session.last_seen.read().await;
        if now.duration_since(last_seen) > stale_after {
            stale.push((subdomain, session));
        }
    }
    let mut cleaned = 0;
    for (subdomain, session) in stale {
        cancel_pending_streams_for_session(state, &session, "stale_session").await;
        let _ = session
            .tx
            .send(Frame::new(FrameType::Goaway, 0, b"stale_session".to_vec()))
            .await;
        if state
            .remove_session(&subdomain, &session.session_id)
            .await
            .is_some()
        {
            cleaned += 1;
        } else {
            continue;
        }
        if state.has_active_tunnel_session(&session.tunnel_id).await {
            sqlx::query(
                "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = 'stale_session' WHERE id = ?1 AND disconnected_at IS NULL",
            )
            .bind(&session.session_id)
            .execute(&state.pool)
            .await?;
        } else {
            sqlx::query(
                "UPDATE sessions SET disconnected_at = CURRENT_TIMESTAMP, disconnect_reason = 'stale_session' WHERE id = ?1 AND disconnected_at IS NULL; \
                 UPDATE tunnels SET status = 'disconnected', disconnected_at = CURRENT_TIMESTAMP, claim_expires_at = datetime('now', ?3) WHERE id = ?2 AND status = 'connected'",
            )
            .bind(&session.session_id)
            .bind(&session.tunnel_id)
            .bind(SUBDOMAIN_CLAIM_AFTER_DISCONNECT)
            .execute(&state.pool)
            .await?;
        }
    }
    Ok(cleaned)
}

async fn cancel_pending_streams_for_session(
    state: &AppState,
    session: &crate::state::ActiveSession,
    reason: &str,
) {
    let streams = state
        .remove_pending_streams_for_session(&session.tunnel_id, &session.session_id)
        .await;
    for (stream_id, _) in streams {
        let _ = session
            .tx
            .send(Frame::new(
                FrameType::Cancel,
                stream_id,
                reason.as_bytes().to_vec(),
            ))
            .await;
    }
}
