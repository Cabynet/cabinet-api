use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::infra::http_client::SsrfSafeClient;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{
    BookmarkListItem, BookmarkListResponse, BookmarkPreviewsQuery, BookmarkPreviewsResponse,
    CreateBookmarkRequest, DuplicateCleanupResponse, DuplicateReportResponse,
    LinkHealthCheckResponse, ListBookmarksQuery, UpdateBookmarkRequest,
};
use super::service::BookmarksService;
use crate::modules::notes::handlers::{get_note_handler, update_note_handler};

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_bookmarks).post(create_bookmark))
        .route("/previews", get(get_previews))
        .route("/proxy-image", get(proxy_image_handler))
        .route("/duplicates", get(get_duplicates_handler))
        .route("/duplicates/cleanup", axum::routing::post(cleanup_duplicates_handler))
        .route("/export", get(crate::modules::tasks::handlers::export_bookmarks_handler))
        .route(
            "/:id",
            get(get_bookmark)
                .patch(update_bookmark)
                .delete(delete_bookmark),
        )
        .route("/:id/check-link", axum::routing::post(check_link_handler))
        .route("/:id/note", get(get_note_handler).put(update_note_handler))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/previews",
    params(BookmarkPreviewsQuery),
    responses(
        (status = 200, description = "Lightweight preview metadata for given IDs", body = BookmarkPreviewsResponse)
    ),
    tag = "bookmarks"
)]
pub async fn get_previews(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<BookmarkPreviewsQuery>,
) -> Result<Json<BookmarkPreviewsResponse>, AppError> {
    let ids: Vec<Uuid> = query
        .ids
        .split(',')
        .filter_map(|s| s.trim().parse::<Uuid>().ok())
        .collect();

    let service = BookmarksService::new(pool, None);
    let previews = service.get_previews(user.id, ids).await?;
    Ok(Json(BookmarkPreviewsResponse { previews }))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks",
    params(ListBookmarksQuery),
    responses(
        (status = 200, description = "Keyset cursor paginated list of bookmarks", body = BookmarkListResponse)
    ),
    tag = "bookmarks"
)]
pub async fn list_bookmarks(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<ListBookmarksQuery>,
) -> Result<Json<BookmarkListResponse>, AppError> {
    let service = BookmarksService::new(pool, None);
    let resp = service.list_bookmarks(user.id, query).await?;
    Ok(Json(resp))
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks",
    request_body = CreateBookmarkRequest,
    responses(
        (status = 201, description = "Bookmark created successfully", body = BookmarkListItem)
    ),
    tag = "bookmarks"
)]
pub async fn create_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<CreateBookmarkRequest>,
) -> Result<(StatusCode, Json<BookmarkListItem>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let config = crate::config::AppConfig::from_env().ok();
    let redis = if let Some(ref cfg) = config {
        crate::infra::redis::create_redis_pool(cfg).await.ok()
    } else {
        None
    };

    let service = BookmarksService::new(pool, redis);
    let item = service.create_bookmark(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(item)))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/{id}",
    params(
        ("id" = Uuid, Path, description = "Bookmark ID")
    ),
    responses(
        (status = 200, description = "Bookmark details", body = BookmarkListItem)
    ),
    tag = "bookmarks"
)]
pub async fn get_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<BookmarkListItem>, AppError> {
    let service = BookmarksService::new(pool, None);
    let item = service.get_bookmark(user.id, id).await?;
    Ok(Json(item))
}

#[utoipa::path(
    patch,
    path = "/v1/bookmarks/{id}",
    params(
        ("id" = Uuid, Path, description = "Bookmark ID")
    ),
    request_body = UpdateBookmarkRequest,
    responses(
        (status = 200, description = "Updated bookmark", body = BookmarkListItem)
    ),
    tag = "bookmarks"
)]
pub async fn update_bookmark(
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

#[utoipa::path(
    delete,
    path = "/v1/bookmarks/{id}",
    params(
        ("id" = Uuid, Path, description = "Bookmark ID")
    ),
    responses(
        (status = 204, description = "Bookmark deleted successfully")
    ),
    tag = "bookmarks"
)]
pub async fn delete_bookmark(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let service = BookmarksService::new(pool, None);
    service.delete_bookmark(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/duplicates",
    responses(
        (status = 200, description = "Groups of duplicate bookmarks", body = DuplicateReportResponse)
    ),
    tag = "bookmarks"
)]
pub async fn get_duplicates_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<DuplicateReportResponse>, AppError> {
    let service = BookmarksService::new(pool, None);
    let report = service.get_duplicates(user.id).await?;
    Ok(Json(report))
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/duplicates/cleanup",
    responses(
        (status = 200, description = "Duplicate cleanup summary", body = DuplicateCleanupResponse)
    ),
    tag = "bookmarks"
)]
pub async fn cleanup_duplicates_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<DuplicateCleanupResponse>, AppError> {
    let service = BookmarksService::new(pool, None);
    let res = service.cleanup_duplicates(user.id).await?;
    Ok(Json(res))
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/{id}/check-link",
    params(
        ("id" = Uuid, Path, description = "Bookmark ID")
    ),
    responses(
        (status = 200, description = "Link health check result", body = LinkHealthCheckResponse)
    ),
    tag = "bookmarks"
)]
pub async fn check_link_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<LinkHealthCheckResponse>, AppError> {
    let service = BookmarksService::new(pool, None);
    let res = service.check_bookmark_health(user.id, id).await?;
    Ok(Json(res))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
pub struct ProxyImageQuery {
    pub url: String,
    pub v: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/proxy-image",
    params(ProxyImageQuery),
    responses(
        (status = 200, description = "Proxied image binary")
    ),
    tag = "bookmarks"
)]
pub async fn proxy_image_handler(
    Query(query): Query<ProxyImageQuery>,
) -> Result<impl IntoResponse, AppError> {
    let client = SsrfSafeClient::new()?;
    let resp = client.get_safe(&query.url).await?;

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("image/jpeg")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400, immutable"),
    );

    Ok((StatusCode::OK, headers, bytes))
}
