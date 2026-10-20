use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("cache error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("{0}")]
    Unauthorized(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("not found")]
    NotFound,

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AppError::ServiceUnavailable(_) => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
            }
            AppError::Database(_) | AppError::Redis(_) | AppError::Io(_) | AppError::Other(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };

        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }

        let body = ErrorBody {
            error: code,
            message: self.to_string(),
        };

        (status, Json(body)).into_response()
    }
}
