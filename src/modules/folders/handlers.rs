use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{CreateFolderRequest, FolderDto, UpdateFolderRequest};
use super::service::FoldersService;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicFolderDto {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicBookmarkItem {
    pub id: Uuid,
    pub title: String,
    pub raw_url: Option<String>,
    pub normalized_domain: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicShareResponse {
    pub folder: PublicFolderDto,
    pub bookmarks: Vec<PublicBookmarkItem>,
}

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_folders).post(create_folder))
        .route("/:id", put(update_folder).delete(delete_folder))
        .route("/:id/share", post(enable_share).delete(revoke_share))
}

pub fn public_share_router() -> Router<PgPool> {
    Router::new().route("/:token", get(get_public_share))
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

pub async fn get_public_share(
    State(pool): State<PgPool>,
    Path(token): Path<String>,
) -> Result<Json<PublicShareResponse>, AppError> {
    let service = FoldersService::new(pool.clone());
    let (folder, owner_id) = service.get_shared_folder(&token).await?;

    // Fetch bookmarks for this folder, excluding notes, archived items, and deleted items
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            DateTime<Utc>,
        ),
    >(
        r#"
        SELECT id, title, raw_url, normalized_domain, description, site_name, thumbnail_url, favicon_url, created_at
        FROM bookmarks
        WHERE folder_id = $1
          AND user_id = $2
          AND deleted_at IS NULL
          AND is_archived = FALSE
          AND item_type != 'NOTE'
        ORDER BY is_pinned DESC, position ASC, created_at DESC
        "#,
    )
    .bind(folder.id)
    .bind(owner_id)
    .fetch_all(&pool)
    .await?;

    let bookmarks = rows
        .into_iter()
        .map(|r| PublicBookmarkItem {
            id: r.0,
            title: r.1,
            raw_url: r.2,
            normalized_domain: r.3,
            description: r.4,
            site_name: r.5,
            thumbnail_url: r.6,
            favicon_url: r.7,
            created_at: r.8,
        })
        .collect();

    Ok(Json(PublicShareResponse {
        folder: PublicFolderDto {
            id: folder.id,
            name: folder.name,
            description: folder.description,
        },
        bookmarks,
    }))
}
