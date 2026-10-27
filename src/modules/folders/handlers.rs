use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{CreateFolderRequest, FolderDto, UpdateFolderRequest};
use super::service::FoldersService;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_folders).post(create_folder))
        .route("/:id", put(update_folder).delete(delete_folder))
        .route("/:id/share", post(enable_share).delete(revoke_share))
}

async fn list_folders(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<FolderDto>>, AppError> {
    let service = FoldersService::new(pool);
    let folders = service.list_folders(user.id).await?;
    Ok(Json(folders))
}

async fn create_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<CreateFolderRequest>,
) -> Result<(StatusCode, Json<FolderDto>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = FoldersService::new(pool);
    let folder = service.create_folder(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(folder)))
}

async fn update_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
    Json(payload): Json<UpdateFolderRequest>,
) -> Result<Json<FolderDto>, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = FoldersService::new(pool);
    let folder = service.update_folder(user.id, folder_id, payload).await?;
    Ok(Json(folder))
}

async fn delete_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let service = FoldersService::new(pool);
    service.delete_folder(user.id, folder_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn enable_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let service = FoldersService::new(pool);
    let token = service.enable_share(user.id, folder_id).await?;
    Ok(Json(serde_json::json!({
        "share_token": token,
        "share_url": format!("/share/{}", token)
    })))
}

async fn revoke_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let service = FoldersService::new(pool);
    service.revoke_share(user.id, folder_id).await?;
    Ok(Json(serde_json::json!({ "revoked": true })))
}
