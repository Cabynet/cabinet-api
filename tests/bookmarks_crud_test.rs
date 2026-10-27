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
        bookmarks::{
            dto::{BookmarkListResponse, CreateBookmarkRequest, UpdateBookmarkRequest},
            handlers::router as bookmarks_router,
        },
        folders::{
            dto::{CreateFolderRequest, FolderDto},
            handlers::router as folders_router,
        },
        tags::{
            dto::{CreateTagRequest, TagDto},
            handlers::router as tags_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_bookmarks_folders_tags_crud_and_cursor_pagination() {
    let mut config = AppConfig::from_env().expect("config");
    config.database_url = "postgresql://postgres:postgrespassword@localhost:5432/cabinet".to_string();

    let pool = match create_db_pool(&config).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .nest("/v1/folders", folders_router())
        .nest("/v1/tags", tags_router())
        .with_state(pool);

    // 1. Register test user
    let email = format!("user_{}@example.com", Uuid::now_v7());
    let register_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Bookmark Tester".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(register_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Create Folders (Tree structure: Parent -> Child)
    let create_parent_req = Request::builder()
        .method("POST")
        .uri("/v1/folders")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateFolderRequest {
                name: "Tech".to_string(),
                description: Some("Tech articles".to_string()),
                parent_id: None,
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_parent = app.clone().oneshot(create_parent_req).await.unwrap();
    assert_eq!(resp_parent.status(), StatusCode::CREATED);
    let parent_bytes = axum::body::to_bytes(resp_parent.into_body(), 1024 * 64).await.unwrap();
    let parent_folder: FolderDto = serde_json::from_slice(&parent_bytes).unwrap();

    let create_child_req = Request::builder()
        .method("POST")
        .uri("/v1/folders")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateFolderRequest {
                name: "Rust".to_string(),
                description: Some("Rust lang folder".to_string()),
                parent_id: Some(parent_folder.id),
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_child = app.clone().oneshot(create_child_req).await.unwrap();
    assert_eq!(resp_child.status(), StatusCode::CREATED);

    // 3. Create Tag
    let create_tag_req = Request::builder()
        .method("POST")
        .uri("/v1/tags")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateTagRequest {
                name: "High Performance".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_tag = app.clone().oneshot(create_tag_req).await.unwrap();
    assert_eq!(resp_tag.status(), StatusCode::CREATED);
    let tag_bytes = axum::body::to_bytes(resp_tag.into_body(), 1024 * 64).await.unwrap();
    let tag_dto: TagDto = serde_json::from_slice(&tag_bytes).unwrap();
    assert_eq!(tag_dto.slug, "high-performance");

    // 4. Test Save-First Instant Write & Idempotency (client_request_id)
    let client_req_id = Uuid::now_v7();
    let create_bm_payload = CreateBookmarkRequest {
        url: Some("https://www.rust-lang.org/learn?utm_source=twitter&utm_medium=social".to_string()),
        title: Some("Rust Official Learning".to_string()),
        description: Some("Learn Rust language".to_string()),
        folder_id: Some(parent_folder.id),
        item_type: Some("LINK".to_string()),
        client_request_id: Some(client_req_id),
        tags: Some(vec!["high-performance".to_string(), "backend".to_string()]),
    };

    let create_bm_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&create_bm_payload).unwrap()))
        .unwrap();

    let resp_bm = app.clone().oneshot(create_bm_req).await.unwrap();
    assert_eq!(resp_bm.status(), StatusCode::CREATED);
    let bm_bytes = axum::body::to_bytes(resp_bm.into_body(), 1024 * 64).await.unwrap();
    let bm_item: cabinet_api::modules::bookmarks::dto::BookmarkListItem =
        serde_json::from_slice(&bm_bytes).unwrap();

    assert_eq!(bm_item.normalized_domain.as_deref(), Some("rust-lang.org"));
    assert_eq!(bm_item.metadata_status, "PENDING");
    assert_eq!(bm_item.tags.len(), 2);
    let initial_bm_id = bm_item.id;

    // Retry with SAME client_request_id -> must be idempotent (return same ID, no duplicate!)
    let retry_bm_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&create_bm_payload).unwrap()))
        .unwrap();

    let resp_retry = app.clone().oneshot(retry_bm_req).await.unwrap();
    assert_eq!(resp_retry.status(), StatusCode::CREATED);
    let retry_bytes = axum::body::to_bytes(resp_retry.into_body(), 1024 * 64).await.unwrap();
    let retry_item: cabinet_api::modules::bookmarks::dto::BookmarkListItem =
        serde_json::from_slice(&retry_bytes).unwrap();
    assert_eq!(retry_item.id, initial_bm_id);

    // 5. Create 4 more bookmarks to test Keyset Cursor Pagination
    for i in 1..=4 {
        let req_more = Request::builder()
            .method("POST")
            .uri("/v1/bookmarks")
            .header(header::AUTHORIZATION, &auth_header)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&CreateBookmarkRequest {
                    url: Some(format!("https://example.com/article/{}", i)),
                    title: Some(format!("Article {}", i)),
                    description: None,
                    folder_id: Some(parent_folder.id),
                    item_type: Some("LINK".to_string()),
                    client_request_id: Some(Uuid::now_v7()),
                    tags: None,
                })
                .unwrap(),
            ))
            .unwrap();
        let _ = app.clone().oneshot(req_more).await.unwrap();
    }

    // 6. Test Keyset Pagination (Page 1 limit=2)
    let list_req_p1 = Request::builder()
        .method("GET")
        .uri("/v1/bookmarks?limit=2")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_p1 = app.clone().oneshot(list_req_p1).await.unwrap();
    assert_eq!(resp_p1.status(), StatusCode::OK);
    let p1_bytes = axum::body::to_bytes(resp_p1.into_body(), 1024 * 64).await.unwrap();
    let page1: BookmarkListResponse = serde_json::from_slice(&p1_bytes).unwrap();

    assert_eq!(page1.items.len(), 2);
    assert!(page1.has_more);
    let next_cursor = page1.next_cursor.expect("next_cursor must be present");

    // Page 2 using cursor
    let list_req_p2 = Request::builder()
        .method("GET")
        .uri(format!("/v1/bookmarks?limit=2&cursor={}", next_cursor))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_p2 = app.clone().oneshot(list_req_p2).await.unwrap();
    assert_eq!(resp_p2.status(), StatusCode::OK);
    let p2_bytes = axum::body::to_bytes(resp_p2.into_body(), 1024 * 64).await.unwrap();
    let page2: BookmarkListResponse = serde_json::from_slice(&p2_bytes).unwrap();

    assert_eq!(page2.items.len(), 2);
    // Verify no overlap between Page 1 and Page 2
    assert_ne!(page1.items[0].id, page2.items[0].id);
    assert_ne!(page1.items[1].id, page2.items[0].id);

    // 7. Test Bookmark Update
    let update_req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/bookmarks/{}", initial_bm_id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateBookmarkRequest {
                title: Some("Updated Title".to_string()),
                description: None,
                folder_id: None,
                is_pinned: Some(true),
                is_favorite: Some(true),
                is_archived: None,
                tags: None,
            })
            .unwrap(),
        ))
        .unwrap();

    let resp_update = app.clone().oneshot(update_req).await.unwrap();
    assert_eq!(resp_update.status(), StatusCode::OK);
    let updated_bytes = axum::body::to_bytes(resp_update.into_body(), 1024 * 64).await.unwrap();
    let updated_bm: cabinet_api::modules::bookmarks::dto::BookmarkListItem =
        serde_json::from_slice(&updated_bytes).unwrap();
    assert_eq!(updated_bm.title, "Updated Title");
    assert!(updated_bm.is_pinned);
    assert!(updated_bm.is_favorite);

    // 8. Test Bookmark Delete
    let delete_req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/bookmarks/{}", initial_bm_id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_delete = app.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(resp_delete.status(), StatusCode::NO_CONTENT);

    // Verify deleted bookmark is no longer in active listing
    let get_deleted = Request::builder()
        .method("GET")
        .uri(format!("/v1/bookmarks/{}", initial_bm_id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let resp_get_del = app.clone().oneshot(get_deleted).await.unwrap();
    assert_eq!(resp_get_del.status(), StatusCode::NOT_FOUND);
}
