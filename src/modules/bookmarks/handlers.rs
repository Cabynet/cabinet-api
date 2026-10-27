use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{
    BookmarkListItem, BookmarkListResponse, CreateBookmarkRequest, ListBookmarksQuery,
    UpdateBookmarkRequest,
};
use super::service::BookmarksService;
use crate::modules::notes::handlers::{get_note_handler, update_note_handler};

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_bookmarks).post(create_bookmark))
        .route(
            "/:id",
            get(get_bookmark)
                .patch(update_bookmark)
                .delete(delete_bookmark),
        )
        .route("/:id/note", get(get_note_handler).put(update_note_handler))
}

async fn list_bookmarks(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<ListBookmarksQuery>,
) -> Result<Json<BookmarkListResponse>, AppError> {
    let service = BookmarksService::new(pool, None);
    let resp = service.list_bookmarks(user.id, query).await?;
    Ok(Json(resp))
}

async fn create_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<CreateBookmarkRequest>,
) -> Result<(StatusCode, Json<BookmarkListItem>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = BookmarksService::new(pool, None);
    let item = service.create_bookmark(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(item)))
}

async fn get_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<BookmarkListItem>, AppError> {
    let service = BookmarksService::new(pool, None);
    let item = service.get_bookmark(user.id, id).await?;
    Ok(Json(item))
}

async fn update_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateBookmarkRequest>,
) -> Result<Json<BookmarkListItem>, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = BookmarksService::new(pool, None);
    let item = service.update_bookmark(user.id, id, payload).await?;
    Ok(Json(item))
}

async fn delete_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let service = BookmarksService::new(pool, None);
    service.delete_bookmark(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
