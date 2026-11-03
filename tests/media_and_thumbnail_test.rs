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
        bookmarks::handlers::router as bookmarks_router,
        media::{
            dto::{ConfirmUploadResponse, PresignUploadRequest, PresignUploadResponse},
            handlers::router as media_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_media_presign_quota_validation_and_confirm() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/media", media_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .with_state(pool.clone());

    // 1. Register test user
    let email = format!("media_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Media Tester".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // Set user quota strictly to 100 KB (102,400 bytes) to test quota enforcement
    sqlx::query("UPDATE users SET storage_quota_bytes = 102400 WHERE id = $1")
        .bind(session.user.id)
        .execute(&pool)
        .await
        .unwrap();

    // 2. Test Presign Exceeding Quota (150 KB > 100 KB) -> Should return 413 / QUOTA_EXCEEDED
    let presign_exceeded_req = Request::builder()
        .method("POST")
        .uri("/v1/media/presign")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&PresignUploadRequest {
                filename: "large-document.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                size_bytes: 150000,
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_exceeded = app.clone().oneshot(presign_exceeded_req).await.unwrap();
    assert_eq!(resp_exceeded.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // 3. Test Presign Within Quota (50 KB < 100 KB) -> Should return 201 Created
    let presign_ok_req = Request::builder()
        .method("POST")
        .uri("/v1/media/presign")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&PresignUploadRequest {
                filename: "my-screenshot.png".to_string(),
                mime_type: "image/png".to_string(),
                size_bytes: 51200, // 50 KB
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_ok = app.clone().oneshot(presign_ok_req).await.unwrap();
    assert_eq!(resp_ok.status(), StatusCode::CREATED);
    let ok_bytes = axum::body::to_bytes(resp_ok.into_body(), 1024 * 64).await.unwrap();
    let presign_res: PresignUploadResponse = serde_json::from_slice(&ok_bytes).unwrap();

    assert!(presign_res.upload_url.contains("my-screenshot.png"));
    assert_eq!(presign_res.expires_seconds, 600);

    // Confirmation must reject an object which was never uploaded.
    let missing = app.clone().oneshot(Request::builder().method("POST").uri(format!("/v1/media/{}/confirm",presign_res.asset_id)).header(header::AUTHORIZATION,&auth_header).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    reqwest::Client::new().put(&presign_res.upload_url).header("Content-Type","image/png").body(vec![0u8;51200]).send().await.unwrap().error_for_status().unwrap();
    // 4. Test Confirm Upload
    let confirm_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/media/{}/confirm", presign_res.asset_id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_confirm = app.clone().oneshot(confirm_req).await.unwrap();
    assert_eq!(resp_confirm.status(), StatusCode::OK);
    let confirm_bytes = axum::body::to_bytes(resp_confirm.into_body(), 1024 * 64).await.unwrap();
    let confirm_res: ConfirmUploadResponse = serde_json::from_slice(&confirm_bytes).unwrap();
    assert_eq!(confirm_res.status, "READY");

    // Verify storage_used_bytes increased in DB
    let used_bytes: i64 = sqlx::query_scalar("SELECT storage_used_bytes FROM users WHERE id = $1")
        .bind(session.user.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(used_bytes, 51200);

    // Permanently deleting the file bookmark frees the quota and removes the stored object.
    assert_eq!(reqwest::get(&confirm_res.public_url).await.unwrap().status(), 200);
    let create = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({"url": confirm_res.public_url, "item_type": "FILE", "title": "shot"}).to_string()))
        .unwrap();
    assert_eq!(app.clone().oneshot(create).await.unwrap().status(), StatusCode::CREATED);
    // Creating the bookmark links the uploaded asset to it; purge relies on this link.
    let linked: Option<Uuid> = sqlx::query_scalar("SELECT bookmark_id FROM media_assets WHERE id = $1")
        .bind(presign_res.asset_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let bookmark_id: Uuid = sqlx::query_scalar("SELECT id FROM bookmarks WHERE user_id = $1")
        .bind(session.user.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(linked, Some(bookmark_id));
    sqlx::query("UPDATE bookmarks SET deleted_at = now() WHERE user_id = $1")
        .bind(session.user.id)
        .execute(&pool)
        .await
        .unwrap();
    let empty = Request::builder()
        .method("DELETE")
        .uri("/v1/bookmarks/trash/empty")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.clone().oneshot(empty).await.unwrap().status(), StatusCode::OK);
    let used_after: i64 = sqlx::query_scalar("SELECT storage_used_bytes FROM users WHERE id = $1")
        .bind(session.user.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(used_after, 0);
    assert_eq!(reqwest::get(&confirm_res.public_url).await.unwrap().status(), 404);
}
