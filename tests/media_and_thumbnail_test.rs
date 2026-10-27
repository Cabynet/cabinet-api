use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cabinet_api::{
    config::AppConfig,
    infra::db::create_db_pool,
    modules::{
        auth::{
            dto::{RegisterRequest, SessionResponse},
            handlers::router as auth_router,
        },
        media::{
            dto::{ConfirmUploadResponse, PresignUploadRequest, PresignUploadResponse},
            handlers::router as media_router,
        },
    },
    worker::thumbnail::ThumbnailProcessor,
};
use image::{ImageBuffer, Rgb};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_media_presign_quota_validation_and_confirm() {
    let mut config = AppConfig::from_env().expect("config");
    config.database_url = "postgresql://postgres:postgrespassword@localhost:5432/cabinet".to_string();

    let pool = match create_db_pool(&config).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/media", media_router())
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
}

#[test]
fn test_thumbnail_processor_resizes_to_webp_640px() {
    // 1. Create a 1200x800 test image in memory
    let width = 1200;
    let height = 800;
    let mut img_buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width, height);

    for (x, y, pixel) in img_buf.enumerate_pixels_mut() {
        let r = (x % 256) as u8;
        let g = (y % 256) as u8;
        let b = ((x + y) % 256) as u8;
        *pixel = Rgb([r, g, b]);
    }

    let mut jpeg_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
    img_buf
        .write_to(&mut cursor, image::ImageFormat::Jpeg)
        .unwrap();

    // 2. Compress to WebP with max 640px dimension
    let webp_bytes = ThumbnailProcessor::compress_to_webp(&jpeg_bytes, 640).unwrap();

    // Verify RIFF and WEBP magic bytes
    assert_eq!(&webp_bytes[0..4], b"RIFF");
    assert_eq!(&webp_bytes[8..12], b"WEBP");

    // 3. Decode WebP and verify dimensions
    let decoded = image::load_from_memory(&webp_bytes).unwrap();
    assert!(decoded.width() <= 640);
    assert_eq!(decoded.width(), 640); // 1200x800 scaled down maintains aspect ratio -> 640x427
    assert!(decoded.height() <= 640);
    assert_eq!(decoded.height(), 427);
}
