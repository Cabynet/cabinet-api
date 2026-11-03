use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::Serialize;
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
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/presign", post(presign_upload_handler))
        .route("/:id/confirm", post(confirm_upload_handler))
        .route("/upload", post(upload_file_handler))
        .layer(axum::extract::DefaultBodyLimit::max(60 * 1024 * 1024))
}

pub fn uploads_router() -> Router<PgPool> {
    uploads_router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn uploads_router_routes() -> Router<PgPool> {
    Router::new()
        .route("/", post(upload_file_handler))
        .route("/bookmark", post(upload_bookmark_handler))
        .layer(axum::extract::DefaultBodyLimit::max(60 * 1024 * 1024))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSuccessData {
    pub url: String,
    pub key: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadApiResponse<T> {
    pub success: bool,
    pub data: T,
}

pub async fn upload_file_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<UploadApiResponse<UploadSuccessData>>, AppError> {
    let mut file_bytes = Vec::new();
    let mut filename = "upload".to_string();
    let mut content_type = "application/octet-stream".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            if let Some(fname) = field.file_name() {
                if !fname.is_empty() {
                    filename = fname.to_string();
                }
            }
            if let Some(ct) = field.content_type() {
                if !ct.is_empty() {
                    content_type = ct.to_string();
                }
            }
            file_bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to read file field: {}", e)))?
                .to_vec();
        }
    }

    if file_bytes.is_empty() {
        return Err(AppError::BadRequest("No file provided".to_string()));
    }

    if content_type == "application/octet-stream" {
        let lower = filename.to_ascii_lowercase();
        if lower.ends_with(".pdf") {
            content_type = "application/pdf".to_string();
        } else if lower.ends_with(".png") {
            content_type = "image/png".to_string();
        } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            content_type = "image/jpeg".to_string();
        } else if lower.ends_with(".webp") {
            content_type = "image/webp".to_string();
        } else if lower.ends_with(".gif") {
            content_type = "image/gif".to_string();
        } else if lower.ends_with(".svg") {
            content_type = "image/svg+xml".to_string();
        }
    }

    let config = AppConfig::shared().map_err(AppError::Internal)?;
    let r2 = R2Service::new(config);
    let size = file_bytes.len() as i64;
    let service = MediaService::new(pool, r2);
    let confirmed = service
        .upload_file(user.id, filename.clone(), content_type.clone(), file_bytes)
        .await?;

    Ok(Json(UploadApiResponse {
        success: true,
        data: UploadSuccessData {
            url: confirmed.public_url,
            key: format!("uploads/{}/{}", user.id, confirmed.asset_id),
            filename,
            content_type,
            size,
        },
    }))
}

pub async fn upload_bookmark_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<UploadApiResponse<crate::modules::bookmarks::dto::BookmarkListItem>>, AppError> {
    let mut file_bytes = Vec::new();
    let mut filename = "upload".to_string();
    let mut content_type = "application/octet-stream".to_string();
    let mut title: Option<String> = None;
    let mut description: Option<String> = None;
    let mut folder_id: Option<Uuid> = None;
    let mut tags: Option<Vec<String>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                if let Some(fname) = field.file_name() {
                    if !fname.is_empty() {
                        filename = fname.to_string();
                    }
                }
                if let Some(ct) = field.content_type() {
                    if !ct.is_empty() {
                        content_type = ct.to_string();
                    }
                }
                file_bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file field: {}", e)))?
                    .to_vec();
            }
            "title" => {
                let text = field.text().await.unwrap_or_default();
                if !text.trim().is_empty() {
                    title = Some(text.trim().to_string());
                }
            }
            "description" => {
                let text = field.text().await.unwrap_or_default();
                if !text.trim().is_empty() {
                    description = Some(text.trim().to_string());
                }
            }
            "folderId" | "folder_id" => {
                let text = field.text().await.unwrap_or_default();
                if let Ok(fid) = Uuid::parse_str(text.trim()) {
                    folder_id = Some(fid);
                }
            }
            "tags" => {
                let text = field.text().await.unwrap_or_default();
                let parsed: Vec<String> = text
                    .split(',')
                    .map(|s| s.trim().trim_start_matches('#').to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !parsed.is_empty() {
                    tags = Some(parsed);
                }
            }
            _ => {}
        }
    }

    if file_bytes.is_empty() {
        return Err(AppError::BadRequest("No file provided".to_string()));
    }

    if content_type == "application/octet-stream" {
        let lower = filename.to_ascii_lowercase();
        if lower.ends_with(".pdf") {
            content_type = "application/pdf".to_string();
        } else if lower.ends_with(".png") {
            content_type = "image/png".to_string();
        } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            content_type = "image/jpeg".to_string();
        } else if lower.ends_with(".webp") {
            content_type = "image/webp".to_string();
        } else if lower.ends_with(".gif") {
            content_type = "image/gif".to_string();
        } else if lower.ends_with(".svg") {
            content_type = "image/svg+xml".to_string();
        }
    }

    let config = AppConfig::shared().map_err(AppError::Internal)?;
    let r2 = R2Service::new(config);
    let media_service = MediaService::new(pool.clone(), r2);
    let confirmed = media_service
        .upload_file(user.id, filename.clone(), content_type.clone(), file_bytes)
        .await?;

    let is_pdf = content_type == "application/pdf" || filename.to_ascii_lowercase().ends_with(".pdf");
    let is_img = content_type.starts_with("image/");
    let content_type_str = if is_pdf {
        "PDF"
    } else if is_img {
        "IMAGE"
    } else {
        "FILE"
    };

    let bookmarks_service = crate::modules::bookmarks::service::BookmarksService::new(pool, None);
    let bookmark = bookmarks_service
        .create_bookmark(
            user.id,
            crate::modules::bookmarks::dto::CreateBookmarkRequest {
                url: Some(confirmed.public_url.clone()),
                item_type: Some("FILE".to_string()),
                title: Some(title.unwrap_or(filename)),
                description,
                folder_id,
                tags,
                client_request_id: None,
                note_document: None,
                notes_plain: None,
                drawing: None,
                thumbnail_url: if is_img { Some(confirmed.public_url) } else { None },
                favicon_url: None,
                content_type: Some(content_type_str.to_string()),
            },
        )
        .await?;

    Ok(Json(UploadApiResponse {
        success: true,
        data: bookmark,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/media/presign",
    request_body = PresignUploadRequest,
    responses(
        (status = 201, description = "Presigned PUT URL generated successfully", body = PresignUploadResponse),
        (status = 413, description = "Storage quota exceeded")
    ),
    tag = "media"
)]
pub async fn presign_upload_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<PresignUploadRequest>,
) -> Result<(StatusCode, Json<PresignUploadResponse>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let config = AppConfig::shared().map_err(AppError::Internal)?;
    let r2 = R2Service::new(config);
    let service = MediaService::new(pool, r2);

    let res = service.presign_upload(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(res)))
}

#[utoipa::path(
    post,
    path = "/v1/media/{id}/confirm",
    params(
        ("id" = Uuid, Path, description = "Media Asset ID")
    ),
    responses(
        (status = 200, description = "Media upload confirmed and quota updated", body = ConfirmUploadResponse)
    ),
    tag = "media"
)]
pub async fn confirm_upload_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ConfirmUploadResponse>, AppError> {
    let config = AppConfig::shared().map_err(AppError::Internal)?;
    let r2 = R2Service::new(config);
    let service = MediaService::new(pool, r2);

    let res = service.confirm_upload(user.id, id).await?;
    Ok(Json(res))
}
