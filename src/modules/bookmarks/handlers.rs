use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::infra::http_client::SsrfSafeClient;
use crate::modules::auth::middleware::{CurrentUser, OptionalCurrentUser};
use super::dto::{
    BookmarkListItem, BookmarkListResponse, BookmarkPreviewsQuery, BookmarkPreviewsResponse,
    CreateBookmarkRequest, DuplicateCleanupResponse, DuplicateReportResponse,
    LinkHealthCheckResponse, ListBookmarksQuery, UpdateBookmarkRequest,
};
use super::service::BookmarksService;
use crate::modules::notes::handlers::{get_note_handler, update_note_handler};

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_bookmarks).post(create_bookmark))
        .route("/previews", get(get_previews))
        .route("/proxy-image", get(proxy_image_handler))
        .route("/proxy-image/sign", axum::routing::post(sign_proxy_urls_handler))
        .route("/duplicates", get(get_duplicates_handler))
        .route("/duplicates/cleanup", axum::routing::post(cleanup_duplicates_handler))
        .route("/export", get(crate::modules::tasks::handlers::export_bookmarks_handler))
        .merge(super::actions::router())
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

    let service = BookmarksService::new(pool, crate::infra::redis::shared().await);
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
    /// HMAC of `url` from POST /v1/bookmarks/proxy-image/sign.
    pub sig: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignProxyRequest {
    pub urls: Vec<String>,
    #[serde(alias = "share_token")]
    pub share_token: Option<String>,
    /// "image" (default) signs /proxy-image URLs; "page" signs /proxy-page URLs.
    pub kind: Option<String>,
}

/// Returns `{ items: [{ url, signedUrl }] }` with API-relative `/v1/bookmarks/proxy-image?...` URLs.
/// Signed-in users may sign any URL; anonymous callers only URLs of items in the shared folder they name.
pub async fn sign_proxy_urls_handler(
    State(pool): State<PgPool>,
    OptionalCurrentUser(user): OptionalCurrentUser,
    Json(req): Json<SignProxyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.urls.len() > 100 || req.urls.iter().any(|u| u.len() > 2048) {
        return Err(AppError::BadRequest("Too many or too long URLs".into()));
    }
    let page = match req.kind.as_deref() {
        None | Some("image") => false,
        Some("page") => true,
        Some(_) => return Err(AppError::BadRequest("kind must be image or page".into())),
    };
    let allowed: Vec<String> = if let Some(token) = req.share_token {
        let (folder, owner_id) = crate::modules::folders::service::FoldersService::new(pool.clone())
            .get_shared_folder(&token)
            .await?;
        sqlx::query_scalar(
            r#"
            SELECT DISTINCT u FROM bookmarks b, unnest(ARRAY[b.thumbnail_url, b.favicon_url, b.raw_url]) u
            WHERE b.folder_id = $1 AND b.user_id = $2 AND b.deleted_at IS NULL AND u = ANY($3)
            "#,
        )
        .bind(folder.id)
        .bind(owner_id)
        .bind(&req.urls)
        .fetch_all(&pool)
        .await?
    } else if user.is_some() {
        req.urls
    } else {
        return Err(AppError::Unauthorized("Authentication required".into()));
    };

    let items: Vec<_> = allowed
        .iter()
        .map(|url| {
            let (path, sig) = if page {
                ("proxy-page", crate::infra::proxy_sign::sign_page(url))
            } else {
                ("proxy-image", crate::infra::proxy_sign::sign(url))
            };
            serde_json::json!({
                "url": url,
                "signedUrl": format!("/v1/bookmarks/{path}?url={}&sig={sig}", urlencoding::encode(url)),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "items": items })))
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
) -> Result<Response, AppError> {
    // Only URLs this API signed: stops the proxy from being an open fetch relay.
    if !query.sig.as_deref().is_some_and(|sig| crate::infra::proxy_sign::verify(&query.url, sig)) {
        return Err(AppError::Forbidden("Missing or invalid proxy signature".into()));
    }
    // PDFs can be much larger than metadata pages; keep the short default elsewhere.
    let client = SsrfSafeClient::with_timeout(std::time::Duration::from_secs(60))?;
    let is_reddit_share = url::Url::parse(&query.url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case("share.redd.it")))
        .unwrap_or(false);
    let ua = if is_reddit_share {
        Some("WhatsApp/2.24.6.77 A")
    } else {
        None
    };
    let resp = match client.get_safe_with_ua(&query.url, ua).await {
        Ok(resp) => resp,
        Err(AppError::BadRequest(message)) if message.starts_with("HTTP request failed:") => {
            tracing::warn!(error = %message, "Media proxy upstream request failed");
            return Ok(proxy_failure(StatusCode::BAD_GATEWAY));
        }
        Err(error) => return Err(error),
    };
    Ok(proxy_media_response(resp).await)
}

fn proxy_failure(status: StatusCode) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], "Media could not be fetched").into_response()
}

async fn proxy_media_response(mut resp: reqwest::Response) -> Response {
    const MAX_BYTES: usize = 25 * 1024 * 1024;
    if !resp.status().is_success() {
        return proxy_failure(resp.status());
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_ascii_lowercase();
    // Served from the API origin: anything scriptable (html, svg, xml) would run with the session cookie.
    let mime = content_type.split(';').next().unwrap_or("").trim();
    if !(mime == "application/pdf" || (mime.starts_with("image/") && !mime.contains("svg"))) {
        return proxy_failure(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    let mut bytes = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_BYTES => bytes.extend_from_slice(&chunk),
            Ok(Some(_)) => return proxy_failure(StatusCode::PAYLOAD_TOO_LARGE),
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "Media proxy download failed");
                return proxy_failure(StatusCode::BAD_GATEWAY);
            }
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("image/jpeg")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );

    (StatusCode::OK, headers, bytes).into_response()
}

#[cfg(test)]
mod proxy_tests {
    use super::*;

    #[tokio::test]
    async fn proxy_does_not_cache_incomplete_downloads() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n%PDF-").await.unwrap();
        });
        let upstream = reqwest::get(format!("http://{address}/pdf")).await.unwrap();
        let response = proxy_media_response(upstream).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_preserves_errors_without_caching_and_serves_pdf_bytes() {
        let app = Router::new()
            .route("/pdf", get(|| async { ([(header::CONTENT_TYPE, "application/pdf")], "%PDF-1.4\n") }))
            .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
            .route("/busy", get(|| async { StatusCode::TOO_MANY_REQUESTS }))
            .route("/unavailable", get(|| async { StatusCode::SERVICE_UNAVAILABLE }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        for (path, status) in [("missing", StatusCode::NOT_FOUND), ("busy", StatusCode::TOO_MANY_REQUESTS), ("unavailable", StatusCode::SERVICE_UNAVAILABLE)] {
            let upstream = client.get(format!("http://{address}/{path}")).send().await.unwrap();
            let response = proxy_media_response(upstream).await;
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let upstream = client.get(format!("http://{address}/pdf")).send().await.unwrap();
        let response = proxy_media_response(upstream).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/pdf");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "public, max-age=3600");
        let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&bytes[..], b"%PDF-1.4\n");
        server.abort();
    }
}

#[cfg(test)]
mod proxy_filter_tests {
    use super::*;

    fn upstream(content_type: &str, body: Vec<u8>) -> reqwest::Response {
        axum::http::Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn only_images_and_pdfs_are_served_with_hardening_headers() {
        for ct in ["text/html", "image/svg+xml", "application/xml", "application/javascript"] {
            let resp = proxy_media_response(upstream(ct, b"<script>alert(1)</script>".to_vec())).await;
            assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}");
        }
        for ct in ["image/png", "IMAGE/JPEG; charset=x", "application/pdf"] {
            let resp = proxy_media_response(upstream(ct, vec![1, 2, 3])).await;
            assert_eq!(resp.status(), StatusCode::OK, "{ct}");
            assert_eq!(resp.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert!(resp.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().starts_with("sandbox"));
        }
    }

    #[tokio::test]
    async fn oversized_bodies_are_rejected() {
        let resp = proxy_media_response(upstream("image/png", vec![0u8; 26 * 1024 * 1024])).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
