use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Storage quota exceeded")]
    QuotaExceeded,

    #[error("SSRF or invalid target address blocked: {0}")]
    SsrfBlocked(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Internal server error: {0}")]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetails,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetails {
    pub code: String,
    pub message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg.clone()),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg.clone()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg.clone()),
            AppError::QuotaExceeded => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "QUOTA_EXCEEDED",
                "Storage quota exceeded".to_string(),
            ),
            AppError::SsrfBlocked(msg) => (StatusCode::BAD_REQUEST, "SSRF_BLOCKED", msg.clone()),
            AppError::Database(err) => {
                tracing::error!(error = ?err, "Database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DATABASE_ERROR",
                    "A database error occurred".to_string(),
                )
            }
            AppError::Redis(err) => {
                tracing::error!(error = ?err, "Redis error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "REDIS_ERROR",
                    "A caching service error occurred".to_string(),
                )
            }
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "Internal unexpected error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "An unexpected error occurred".to_string(),
                )
            }
        };

        (
            status,
            Json(ErrorResponse {
                error: ErrorDetails {
                    code: code.to_string(),
                    message,
                },
            }),
        )
            .into_response()
    }
}
