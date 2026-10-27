use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{CreateTagRequest, TagDto};
use super::service::TagsService;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_tags).post(create_tag))
        .route("/:id", delete(delete_tag))
}

async fn list_tags(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<TagDto>>, AppError> {
    let service = TagsService::new(pool);
    let tags = service.list_tags(user.id).await?;
    Ok(Json(tags))
}

async fn create_tag(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<CreateTagRequest>,
) -> Result<(StatusCode, Json<TagDto>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = TagsService::new(pool);
    let tag = service.create_tag(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(tag)))
}

async fn delete_tag(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(tag_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let service = TagsService::new(pool);
    service.delete_tag(user.id, tag_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
