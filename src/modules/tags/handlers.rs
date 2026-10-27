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

#[utoipa::path(
    get,
    path = "/v1/tags",
    responses(
        (status = 200, description = "List of user tags with bookmark counts", body = Vec<TagDto>)
    ),
    tag = "tags"
)]
pub async fn list_tags(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<TagDto>>, AppError> {
    let service = TagsService::new(pool);
    let tags = service.list_tags(user.id).await?;
    Ok(Json(tags))
}

#[utoipa::path(
    post,
    path = "/v1/tags",
    request_body = CreateTagRequest,
    responses(
        (status = 201, description = "Tag created successfully", body = TagDto)
    ),
    tag = "tags"
)]
pub async fn create_tag(
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

#[utoipa::path(
    delete,
    path = "/v1/tags/{id}",
    params(
        ("id" = Uuid, Path, description = "Tag ID")
    ),
    responses(
        (status = 204, description = "Tag deleted successfully")
    ),
    tag = "tags"
)]
pub async fn delete_tag(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(tag_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let service = TagsService::new(pool);
    service.delete_tag(user.id, tag_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
