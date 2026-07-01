use crate::state::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use http_tunnel_common::{api::ApiResponse, build_info::BuildInfo};
use serde::Serialize;

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

pub async fn health() -> Json<ApiResponse<Health>> {
    Json(ApiResponse::ok(Health { status: "ok" }))
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
