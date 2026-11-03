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
            dto::CreateBookmarkRequest,
            handlers::router as bookmarks_router,
        },
        folders::{
            dto::{CreateFolderRequest, FolderDto},
            handlers::{public_share_router, router as folders_router, PublicShareResponse},
        },
        search::{
            dto::SearchResponse,
            handlers::router as search_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_postgres_search_and_public_folder_share() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .nest("/v1/folders", folders_router())
        .nest("/v1/search", search_router())
        .nest("/v1/share", public_share_router())
        .with_state(pool.clone());

    // 1. Register test user
    let email = format!("search_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Search Tester".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Create Bookmarks for search testing
    let bm1_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: Some("https://www.postgresql.org/docs/current/performance.html".to_string()),
                title: Some("PostgreSQL Performance Tuning and Indexing".to_string()),
                description: Some("Comprehensive guide to database query optimization and indexes".to_string()),
                folder_id: None,
                item_type: Some("LINK".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let _ = app.clone().oneshot(bm1_req).await.unwrap();

    let bm2_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: Some("https://tokio.rs/blog/axum-framework".to_string()),
                title: Some("Rust Axum Web Framework".to_string()),
                description: Some("Building async microservices with Tokio and Axum".to_string()),
                folder_id: None,
                item_type: Some("LINK".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let _ = app.clone().oneshot(bm2_req).await.unwrap();

    // 3. Test Full-Text Search: "performance tuning"
    let search_fts_req = Request::builder()
        .method("GET")
        .uri("/v1/search?q=performance+tuning")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_fts = app.clone().oneshot(search_fts_req).await.unwrap();
    assert_eq!(resp_fts.status(), StatusCode::OK);
    let fts_bytes = axum::body::to_bytes(resp_fts.into_body(), 1024 * 64).await.unwrap();
    let fts_res: SearchResponse = serde_json::from_slice(&fts_bytes).unwrap();
    assert!(!fts_res.items.is_empty());
    assert!(fts_res.items[0].title.contains("PostgreSQL Performance"));

    // 4. Test Trigram Typo Tolerance: "postgrs" (misspelled postgres)
    let search_trgm_req = Request::builder()
        .method("GET")
        .uri("/v1/search?q=postgrs")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_trgm = app.clone().oneshot(search_trgm_req).await.unwrap();
    assert_eq!(resp_trgm.status(), StatusCode::OK);
    let trgm_bytes = axum::body::to_bytes(resp_trgm.into_body(), 1024 * 64).await.unwrap();
    let trgm_res: SearchResponse = serde_json::from_slice(&trgm_bytes).unwrap();
    assert!(!trgm_res.items.is_empty());
    assert!(trgm_res.items[0].title.contains("PostgreSQL"));

    // 5. Test Public Share Flow & Privacy Rules
    // Create folder "Public Resources"
    let create_folder_req = Request::builder()
        .method("POST")
        .uri("/v1/folders")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateFolderRequest {
                name: "Public Resources".to_string(),
                description: Some("Curated public links".to_string()),
                parent_id: None,
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_folder = app.clone().oneshot(create_folder_req).await.unwrap();
    let folder_bytes = axum::body::to_bytes(resp_folder.into_body(), 1024 * 64).await.unwrap();
    let folder: FolderDto = serde_json::from_slice(&folder_bytes).unwrap();

    // Add Public Bookmark (LINK)
    let pub_bm_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: Some("https://rust-lang.org".to_string()),
                title: Some("Rust Official Site".to_string()),
                description: Some("Public programming language website".to_string()),
                folder_id: Some(folder.id),
                item_type: Some("LINK".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let _ = app.clone().oneshot(pub_bm_req).await.unwrap();

    // Add Private Note item in same folder (MUST NOT BE EXPOSED ON PUBLIC SHARE!)
    let note_bm_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: None,
                title: Some("Confidential API Keys".to_string()),
                description: Some("Secret internal passwords".to_string()),
                folder_id: Some(folder.id),
                item_type: Some("NOTE".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let _ = app.clone().oneshot(note_bm_req).await.unwrap();

    // Enable sharing for folder
    let share_enable_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/folders/{}/share", folder.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_share = app.clone().oneshot(share_enable_req).await.unwrap();
    assert_eq!(resp_share.status(), StatusCode::OK);
    let share_bytes = axum::body::to_bytes(resp_share.into_body(), 1024 * 64).await.unwrap();
    let share_info: serde_json::Value = serde_json::from_slice(&share_bytes).unwrap();
    let token = share_info["shareToken"].as_str().unwrap();

    // 6. Access Public Share WITHOUT authentication
    let public_access_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/share/{}", token))
        .body(Body::empty())
        .unwrap();

    let resp_pub = app.clone().oneshot(public_access_req).await.unwrap();
    assert_eq!(resp_pub.status(), StatusCode::OK);
    let pub_bytes = axum::body::to_bytes(resp_pub.into_body(), 1024 * 64).await.unwrap();
    let pub_res: PublicShareResponse = serde_json::from_slice(&pub_bytes).unwrap();

    assert_eq!(pub_res.folder.name, "Public Resources");
    // Verify only the LINK bookmark is exposed (length 1)
    assert_eq!(pub_res.bookmarks.len(), 1);
    assert_eq!(pub_res.bookmarks[0].title, "Rust Official Site");
    // Privacy assertion: Confidential note was NOT leaked!
    assert!(!pub_res.bookmarks.iter().any(|b| b.title.contains("Confidential")));

    // 7. Revoke share link
    let revoke_req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/folders/{}/share", folder.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_revoke = app.clone().oneshot(revoke_req).await.unwrap();
    assert_eq!(resp_revoke.status(), StatusCode::OK);

    // After revoking, public link must return 404 Not Found
    let pub_after_revoke_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/share/{}", token))
        .body(Body::empty())
        .unwrap();

    let resp_revoked_get = app.clone().oneshot(pub_after_revoke_req).await.unwrap();
    assert_eq!(resp_revoked_get.status(), StatusCode::NOT_FOUND);
}
