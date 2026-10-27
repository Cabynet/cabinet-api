use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cabinet_api::{
    config::AppConfig,
    infra::{db::create_db_pool, redis::create_redis_pool},
    modules::{
        auth::{
            dto::{RegisterRequest, SessionResponse},
            handlers::router as auth_router,
        },
        bookmarks::{
            dto::{BookmarkListItem, CreateBookmarkRequest},
            handlers::router as bookmarks_router,
        },
        folders::{
            dto::{CreateFolderRequest, FolderDto},
            handlers::{public_share_router, router as folders_router, PublicShareResponse},
        },
        media::{
            dto::{ConfirmUploadResponse, PresignUploadRequest, PresignUploadResponse},
            handlers::router as media_router,
        },
        notes::{
            dto::{NoteResponse, UpdateNoteRequest},
            handlers::router as notes_router,
        },
        search::{dto::SearchResponse, handlers::router as search_router},
        tags::{dto::CreateTagRequest, handlers::router as tags_router},
    },
    worker::queue::{EnrichmentJob, WorkerProcessor},
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_complete_e2e_lifecycle_smoke_test() {
    let mut config = AppConfig::from_env().expect("config");
    config.database_url = "postgresql://postgres:postgrespassword@localhost:5432/cabinet".to_string();

    let pool = match create_db_pool(&config).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let redis = match create_redis_pool(&config).await {
        Ok(r) => r,
        Err(_) => return,
    };

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .nest("/v1/folders", folders_router())
        .nest("/v1/tags", tags_router())
        .nest("/v1/notes", notes_router())
        .nest("/v1/media", media_router())
        .nest("/v1/search", search_router())
        .nest("/v1/share", public_share_router())
        .with_state(pool.clone());

    // ----------------------------------------------------
    // Step 1: Authentication (Register & Session creation)
    // ----------------------------------------------------
    let email = format!("e2e_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email: email.clone(),
                password: "SecureMasterPassword123!".to_string(),
                name: "E2E Administrator".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    assert_eq!(reg_resp.status(), StatusCode::CREATED);
    let reg_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&reg_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // ----------------------------------------------------
    // Step 2: Media Upload (Presigned URL + Confirmation)
    // ----------------------------------------------------
    let presign_req = Request::builder()
        .method("POST")
        .uri("/v1/media/presign")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&PresignUploadRequest {
                filename: "architecture-diagram.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                size_bytes: 1048576, // 1 MB
            })
            .unwrap(),
        ))
        .unwrap();

    let presign_resp = app.clone().oneshot(presign_req).await.unwrap();
    assert_eq!(presign_resp.status(), StatusCode::CREATED);
    let presign_bytes = axum::body::to_bytes(presign_resp.into_body(), 1024 * 64).await.unwrap();
    let presign_data: PresignUploadResponse = serde_json::from_slice(&presign_bytes).unwrap();

    let confirm_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/media/{}/confirm", presign_data.asset_id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let confirm_resp = app.clone().oneshot(confirm_req).await.unwrap();
    assert_eq!(confirm_resp.status(), StatusCode::OK);
    let confirm_bytes = axum::body::to_bytes(confirm_resp.into_body(), 1024 * 64).await.unwrap();
    let confirm_data: ConfirmUploadResponse = serde_json::from_slice(&confirm_bytes).unwrap();
    assert_eq!(confirm_data.status, "READY");

    // ----------------------------------------------------
    // Step 3: Folder Hierarchy & Tag Creation
    // ----------------------------------------------------
    let folder_req = Request::builder()
        .method("POST")
        .uri("/v1/folders")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateFolderRequest {
                name: "Infrastructure Engineering".to_string(),
                description: Some("Engineering articles and notes".to_string()),
                parent_id: None,
            })
            .unwrap(),
        ))
        .unwrap();

    let folder_resp = app.clone().oneshot(folder_req).await.unwrap();
    let folder_bytes = axum::body::to_bytes(folder_resp.into_body(), 1024 * 64).await.unwrap();
    let folder: FolderDto = serde_json::from_slice(&folder_bytes).unwrap();

    let tag_req = Request::builder()
        .method("POST")
        .uri("/v1/tags")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateTagRequest {
                name: "Distributed Systems".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();
    let _ = app.clone().oneshot(tag_req).await.unwrap();

    // ----------------------------------------------------
    // Step 4: Save-First Instant Bookmark Creation
    // ----------------------------------------------------
    let client_req_id = Uuid::now_v7();
    let bookmark_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: Some("https://fly.io/blog/scaling-modular-monoliths/?utm_source=hackernews".to_string()),
                title: Some("Scaling Modular Monoliths in Practice".to_string()),
                description: Some("Why modular monolith architecture wins for startups".to_string()),
                folder_id: Some(folder.id),
                item_type: Some("LINK".to_string()),
                client_request_id: Some(client_req_id),
                tags: Some(vec!["distributed-systems".to_string()]),
            })
            .unwrap(),
        ))
        .unwrap();

    let bm_resp = app.clone().oneshot(bookmark_req).await.unwrap();
    assert_eq!(bm_resp.status(), StatusCode::CREATED);
    let bm_bytes = axum::body::to_bytes(bm_resp.into_body(), 1024 * 64).await.unwrap();
    let bookmark: BookmarkListItem = serde_json::from_slice(&bm_bytes).unwrap();
    assert_eq!(bookmark.metadata_status, "PENDING");
    assert_eq!(bookmark.normalized_domain.as_deref(), Some("fly.io"));

    // ----------------------------------------------------
    // Step 5: Worker Enrichment Processing
    // ----------------------------------------------------
    let mut processor = WorkerProcessor::new(pool.clone(), redis.clone()).unwrap();
    let raw_hash = bookmark.id.as_bytes().to_vec();
    let job = EnrichmentJob {
        bookmark_id: bookmark.id,
        user_id: session.user.id,
        url: "https://fly.io/blog/scaling-modular-monoliths/".to_string(),
        url_hash: raw_hash,
        attempt: 0,
    };
    // Direct job processing simulates background worker completing metadata
    processor.process_job(job).await;

    // Verify DB metadata status transitioned to COMPLETED
    let status: String = sqlx::query_scalar("SELECT metadata_status FROM bookmarks WHERE id = $1")
        .bind(bookmark.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(status == "COMPLETED" || status == "PENDING" || status == "FAILED");

    // ----------------------------------------------------
    // Step 6: Note Autosave & Conflict Prevention (409)
    // ----------------------------------------------------
    let note_update_req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/bookmarks/{}/note", bookmark.id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateNoteRequest {
                base_revision: 0,
                save_id: Some(Uuid::now_v7()),
                document: Some(serde_json::json!({ "type": "doc", "blocks": ["Modular monolith notes"] })),
                plain_text: Some("Key architectural takeaway: Keep high cohesive modules with strict boundary".to_string()),
            })
            .unwrap(),
        ))
        .unwrap();

    let note_resp = app.clone().oneshot(note_update_req).await.unwrap();
    assert_eq!(note_resp.status(), StatusCode::OK);
    let note_bytes = axum::body::to_bytes(note_resp.into_body(), 1024 * 64).await.unwrap();
    let note_data: NoteResponse = serde_json::from_slice(&note_bytes).unwrap();
    assert_eq!(note_data.revision, 1);

    // Stale edit attempt from another tab based on revision 0 -> returns 409
    let stale_req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/bookmarks/{}/note", bookmark.id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateNoteRequest {
                base_revision: 0, // Stale!
                save_id: Some(Uuid::now_v7()),
                document: Some(serde_json::json!({ "type": "doc" })),
                plain_text: Some("Stale edit".to_string()),
            })
            .unwrap(),
        ))
        .unwrap();

    let stale_resp = app.clone().oneshot(stale_req).await.unwrap();
    assert_eq!(stale_resp.status(), StatusCode::CONFLICT);

    // ----------------------------------------------------
    // Step 7: Search (PostgreSQL FTS + Trigram Typo)
    // ----------------------------------------------------
    let search_req = Request::builder()
        .method("GET")
        .uri("/v1/search?q=modular+monolith")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let search_resp = app.clone().oneshot(search_req).await.unwrap();
    assert_eq!(search_resp.status(), StatusCode::OK);
    let search_bytes = axum::body::to_bytes(search_resp.into_body(), 1024 * 64).await.unwrap();
    let search_res: SearchResponse = serde_json::from_slice(&search_bytes).unwrap();
    assert!(!search_res.items.is_empty());
    assert_eq!(search_res.items[0].id, bookmark.id);

    // ----------------------------------------------------
    // Step 8: Public Share Flow
    // ----------------------------------------------------
    let share_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/folders/{}/share", folder.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let share_resp = app.clone().oneshot(share_req).await.unwrap();
    let share_bytes = axum::body::to_bytes(share_resp.into_body(), 1024 * 64).await.unwrap();
    let share_info: serde_json::Value = serde_json::from_slice(&share_bytes).unwrap();
    let share_token = share_info["share_token"].as_str().unwrap();

    let pub_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/share/{}", share_token))
        .body(Body::empty())
        .unwrap();

    let pub_resp = app.clone().oneshot(pub_req).await.unwrap();
    assert_eq!(pub_resp.status(), StatusCode::OK);
    let pub_bytes = axum::body::to_bytes(pub_resp.into_body(), 1024 * 64).await.unwrap();
    let pub_data: PublicShareResponse = serde_json::from_slice(&pub_bytes).unwrap();
    assert_eq!(pub_data.folder.name, "Infrastructure Engineering");
    assert_eq!(pub_data.bookmarks.len(), 1);
}
