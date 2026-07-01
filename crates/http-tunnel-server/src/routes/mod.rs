use crate::state::AppState;
use axum::{
    routing::{get, post},
    Router,
};

mod admin;
mod health;
mod metrics;
mod proxy;
mod setup;
mod tunnels;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(proxy::root))
        .route("/admin", get(proxy::admin))
        .route("/admin/setup", get(proxy::setup_page))
        .route("/admin/login", get(proxy::login_page))
        .route("/assets/*path", get(proxy::static_asset))
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/ready", get(health::ready))
        .route("/api/v1/version", get(health::version))
        .route("/metrics", get(metrics::metrics))
        .route("/api/admin/setup/status", get(setup::status))
        .route("/api/admin/setup/init", post(setup::init))
        .route("/api/admin/login", post(admin::login))
        .route("/api/admin/logout", post(admin::logout))
        .route("/api/admin/status", get(admin::status))
        .route("/api/admin/diagnostics", get(admin::diagnostics))
        .route(
            "/api/admin/diagnostics/export",
            get(admin::diagnostics_export),
        )
        .route("/api/admin/alerts", get(admin::alerts))
        .route("/api/admin/sessions", get(admin::sessions))
        .route(
            "/api/admin/sessions/:id/revoke",
            post(admin::revoke_session),
        )
        .route(
            "/api/admin/sessions/revoke-all",
            post(admin::revoke_all_sessions),
        )
        .route(
            "/api/admin/config",
            get(admin::get_config).put(admin::put_config),
        )
        .route("/api/admin/config/schema", get(admin::config_schema))
        .route("/api/admin/config/validate", post(admin::validate_config))
        .route("/api/admin/config/reload", post(admin::reload_config))
        .route(
            "/api/admin/tunnel-create-token/rotate",
            post(admin::rotate_tunnel_create_token),
        )
        .route(
            "/api/admin/tunnel-create-token",
            axum::routing::delete(admin::clear_tunnel_create_token),
        )
        .route(
            "/api/admin/metrics-token/rotate",
            post(admin::rotate_metrics_token),
        )
        .route(
            "/api/admin/metrics-token",
            axum::routing::delete(admin::clear_metrics_token),
        )
        .route(
            "/api/admin/turnstile-secret",
            post(admin::set_turnstile_secret).delete(admin::clear_turnstile_secret),
        )
        .route("/api/admin/password", post(admin::change_password))
        .route("/api/admin/requests", get(admin::recent_requests))
        .route("/api/admin/requests/export", get(admin::requests_export))
        .route("/api/admin/requests/:id", get(admin::request_detail))
        .route(
            "/api/admin/requests/:id/replay",
            post(admin::request_replay),
        )
        .route("/api/admin/events", get(admin::recent_events))
        .route("/api/admin/logs", get(admin::logs))
        .route("/api/admin/audit", get(admin::audit_logs))
        .route("/api/admin/audit/export", get(admin::audit_export))
        .route("/api/admin/version", get(health::version))
        .route("/api/admin/version/full", get(health::version_full))
        .route("/api/admin/restart", post(admin::restart))
        .route("/api/admin/cleanup", post(admin::cleanup))
        .route("/api/admin/backup", post(admin::backup))
        .route("/api/admin/restore/validate", post(admin::restore_validate))
        .route("/api/admin/maintenance", get(admin::maintenance_status))
        .route(
            "/api/admin/maintenance/wal-checkpoint",
            post(admin::wal_checkpoint),
        )
        .route("/api/admin/maintenance/analyze", post(admin::analyze))
        .route("/api/admin/maintenance/vacuum", post(admin::vacuum))
        .route("/api/admin/upgrade", post(admin::upgrade))
        .route("/api/admin/upgrade/validate", post(admin::validate_upgrade))
        .route("/api/admin/upgrade/ws", get(admin::upgrade_ws))
        .route("/api/admin/tunnels", get(tunnels::admin_list))
        .route(
            "/api/admin/tunnels/:id",
            get(tunnels::admin_get)
                .patch(tunnels::admin_patch)
                .delete(tunnels::admin_delete),
        )
        .route("/api/admin/tunnels/:id/detail", get(tunnels::admin_detail))
        .route(
            "/api/admin/tunnels/:id/disconnect",
            post(tunnels::admin_disconnect),
        )
        .route(
            "/api/admin/tunnels/:id/disable",
            post(tunnels::admin_disable),
        )
        .route("/api/admin/tunnels/:id/enable", post(tunnels::admin_enable))
        .route(
            "/api/admin/tunnels/:id/token/rotate",
            post(tunnels::admin_rotate_token),
        )
        .route(
            "/api/admin/tunnels/:id/requests",
            get(tunnels::admin_requests),
        )
        .route("/api/admin/tunnels/:id/events", get(tunnels::admin_events))
        .route("/api/v1/tunnels", post(tunnels::create))
        .route(
            "/api/v1/tunnels/:id",
            get(tunnels::get_tunnel).delete(tunnels::delete_tunnel),
        )
        .route("/api/v1/tunnels/:id/connect", get(tunnels::connect_ws))
        .fallback(proxy::fallback)
}
