use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use http_tunnel_common::api::ApiResponse;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl AppError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            error.to_string(),
        )
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "unauthorized")
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body: ApiResponse<()> = ApiResponse::err(self.code, self.message);
        (self.status, Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
