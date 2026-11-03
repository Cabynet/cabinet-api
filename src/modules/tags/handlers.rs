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
use crate::modules::responses::{DeletedCountResponse, MergedResponse, UpdatedResponse};

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_tags).post(create_tag))
        .route("/unused", delete(delete_unused))
        .route("/:id", delete(delete_tag).patch(rename_tag))
        .route("/:id/merge", axum::routing::post(merge_tag))
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

#[utoipa::path(
    patch,
    path = "/v1/tags/{id}",
    params(
        ("id" = Uuid, Path, description = "Resource ID"),
    ),
    responses(
        (status = 200, description = "OK", body = UpdatedResponse)
    ),
    tag = "tags"
)]
pub async fn rename_tag(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateTagRequest>,
) -> Result<Json<crate::modules::responses::UpdatedResponse>, AppError> {
    req.validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let slug = super::service::slugify(req.name.trim());
    if slug.is_empty() {
        return Err(AppError::BadRequest("Invalid tag name".into()));
    }

    let mut tx = pool.begin().await?;
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tags WHERE user_id = $1 AND slug = $2 AND id <> $3)",
    )
    .bind(user.id)
    .bind(&slug)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    if duplicate {
        return Err(AppError::Conflict(
            "A tag with this name already exists; merge it instead".into(),
        ));
    }

    let r = sqlx::query(
        "UPDATE tags SET name = $3, slug = $4 WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user.id)
    .bind(req.name.trim())
    .bind(slug)
    .execute(&mut *tx)
    .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Tag not found".into()));
    }

    tx.commit().await?;
    Ok(Json(UpdatedResponse { updated: true }))
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequest {
    #[serde(alias = "target_id")]
    target_id: Uuid,
}

#[utoipa::path(
    post,
    path = "/v1/tags/{id}/merge",
    params(
        ("id" = Uuid, Path, description = "Resource ID"),
    ),
    responses(
        (status = 200, description = "OK", body = MergedResponse)
    ),
    tag = "tags"
)]
pub async fn merge_tag(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MergeRequest>,
) -> Result<Json<crate::modules::responses::MergedResponse>, AppError> {
    if id == req.target_id {
        return Err(AppError::BadRequest(
            "Choose a different target tag".into(),
        ));
    }

    let mut tx = pool.begin().await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tags WHERE id = ANY($1) AND user_id = $2 ORDER BY id FOR UPDATE",
    )
    .bind(vec![id, req.target_id])
    .bind(user.id)
    .fetch_all(&mut *tx)
    .await?;

    if ids.len() != 2 {
        return Err(AppError::NotFound("Tag not found".into()));
    }

    sqlx::query(
        r#"
        INSERT INTO bookmark_tags (bookmark_id, tag_id)
        SELECT bt.bookmark_id, $2
        FROM bookmark_tags bt
        JOIN bookmarks b ON b.id = bt.bookmark_id
        WHERE bt.tag_id = $1 AND b.user_id = $3
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(id)
    .bind(req.target_id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM tags WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(Json(MergedResponse { merged: true }))
}

#[utoipa::path(
    delete,
    path = "/v1/tags/unused",
    responses(
        (status = 200, description = "OK", body = DeletedCountResponse)
    ),
    tag = "tags"
)]
pub async fn delete_unused(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<crate::modules::responses::DeletedCountResponse>, AppError> {
    let r = sqlx::query(
        r#"
        DELETE FROM tags t
        WHERE user_id = $1
          AND NOT EXISTS (
              SELECT 1 FROM bookmark_tags bt
              JOIN bookmarks b ON b.id = bt.bookmark_id
              WHERE bt.tag_id = t.id AND b.deleted_at IS NULL
          )
        "#,
    )
    .bind(user.id)
    .execute(&pool)
    .await?;

    Ok(Json(DeletedCountResponse { deleted: r.rows_affected() }))
}
