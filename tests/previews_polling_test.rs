use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
mod common;
use cabinet_api::{
    infra::db::create_db_pool,
    modules::{
        auth::{
            dto::{RegisterRequest, SessionResponse},
            handlers::router as auth_router,
        },
        bookmarks::{
            dto::{BookmarkListItem, BookmarkPreviewsResponse, CreateBookmarkRequest},
            handlers::router as bookmarks_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_bookmark_previews_polling_endpoint() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .with_state(pool.clone());

    // 1. Register test user
    let email = format!("polling_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Polling Tester".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Create Bookmark 1
    let bm1_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: Some("https://example.com/one".to_string()),
                title: Some("Pending Bookmark 1".to_string()),
                description: None,
                folder_id: None,
                item_type: Some("LINK".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let bm1_resp = app.clone().oneshot(bm1_req).await.unwrap();
    let bm1_bytes = axum::body::to_bytes(bm1_resp.into_body(), 1024 * 64).await.unwrap();
    let bm1: BookmarkListItem = serde_json::from_slice(&bm1_bytes).unwrap();

    // 3. Create Bookmark 2
    let bm2_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: Some("https://example.com/two".to_string()),
                title: Some("Pending Bookmark 2".to_string()),
                description: None,
                folder_id: None,
                item_type: Some("LINK".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let bm2_resp = app.clone().oneshot(bm2_req).await.unwrap();
    let bm2_bytes = axum::body::to_bytes(bm2_resp.into_body(), 1024 * 64).await.unwrap();
    let bm2: BookmarkListItem = serde_json::from_slice(&bm2_bytes).unwrap();

    // 4. Test Initial Polling: both in PENDING state
    let poll_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/bookmarks/previews?ids={},{}", bm1.id, bm2.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let poll_resp = app.clone().oneshot(poll_req).await.unwrap();
    assert_eq!(poll_resp.status(), StatusCode::OK);
    let poll_bytes = axum::body::to_bytes(poll_resp.into_body(), 1024 * 64).await.unwrap();
    let poll_data: BookmarkPreviewsResponse = serde_json::from_slice(&poll_bytes).unwrap();

    assert_eq!(poll_data.previews.len(), 2);
    assert_eq!(poll_data.previews[0].metadata_status, "PENDING");
    assert_eq!(poll_data.previews[1].metadata_status, "PENDING");

    // 5. Simulate Worker Completing Bookmark 1
    sqlx::query(
        r#"
        UPDATE bookmarks
        SET metadata_status = 'COMPLETED',
            title = 'Completed Bookmark 1 Title',
            description = 'Enriched description',
            thumbnail_url = 'https://cdn.example.com/thumbs/1.webp',
            favicon_url = 'https://example.com/favicon.ico'
        WHERE id = $1
        "#,
    )
    .bind(bm1.id)
    .execute(&pool)
    .await
    .unwrap();

    // 6. Test Polling Again: Bookmark 1 is COMPLETED, Bookmark 2 is PENDING
    let poll2_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/bookmarks/previews?ids={},{}", bm1.id, bm2.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let poll2_resp = app.clone().oneshot(poll2_req).await.unwrap();
    assert_eq!(poll2_resp.status(), StatusCode::OK);
    let poll2_bytes = axum::body::to_bytes(poll2_resp.into_body(), 1024 * 64).await.unwrap();
    let poll2_data: BookmarkPreviewsResponse = serde_json::from_slice(&poll2_bytes).unwrap();

    let p1 = poll2_data.previews.iter().find(|p| p.id == bm1.id).unwrap();
    assert_eq!(p1.metadata_status, "COMPLETED");
    assert_eq!(p1.title, "Completed Bookmark 1 Title");
    assert_eq!(p1.thumbnail_url.as_deref(), Some("https://cdn.example.com/thumbs/1.webp"));
    assert_eq!(p1.favicon_url.as_deref(), Some("https://example.com/favicon.ico"));

    let p2 = poll2_data.previews.iter().find(|p| p.id == bm2.id).unwrap();
    assert_eq!(p2.metadata_status, "PENDING");
}
