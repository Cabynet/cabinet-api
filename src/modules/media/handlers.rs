use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::config::AppConfig;
use crate::errors::AppError;
use crate::infra::r2::R2Service;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{ConfirmUploadResponse, PresignUploadRequest, PresignUploadResponse};
use super::service::MediaService;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/presign", post(presign_upload_handler))
        .route("/:id/confirm", post(confirm_upload_handler))
}

pub async fn presign_upload_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<PresignUploadRequest>,
) -> Result<(StatusCode, Json<PresignUploadResponse>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let config = AppConfig::from_env().map_err(|e| AppError::Internal(e))?;
    let r2 = R2Service::new(&config);
    let service = MediaService::new(pool, r2);

    let res = service.presign_upload(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(res)))
}

pub async fn confirm_upload_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ConfirmUploadResponse>, AppError> {
    let config = AppConfig::from_env().map_err(|e| AppError::Internal(e))?;
    let r2 = R2Service::new(&config);
    let service = MediaService::new(pool, r2);

    let res = service.confirm_upload(user.id, id).await?;
    Ok(Json(res))
}
