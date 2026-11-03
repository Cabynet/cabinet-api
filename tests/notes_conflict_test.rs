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
            dto::{CreateBookmarkRequest, BookmarkListItem},
            handlers::router as bookmarks_router,
        },
        notes::{
            dto::{NoteResponse, UpdateNoteRequest},
            handlers::router as notes_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_notes_optimistic_concurrency_and_conflict_409() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .nest("/v1/notes", notes_router())
        .with_state(pool.clone());

    // 1. Register test user
    let email = format!("notes_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Note Editor".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Create Note Item
    let create_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&CreateBookmarkRequest {
                url: None,
                title: Some("My Architecture Note".to_string()),
                description: None,
                folder_id: None,
                item_type: Some("NOTE".to_string()),
                client_request_id: Some(Uuid::now_v7()),
                tags: None,
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();

    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let bm_bytes = axum::body::to_bytes(create_resp.into_body(), 1024 * 64).await.unwrap();
    let item: BookmarkListItem = serde_json::from_slice(&bm_bytes).unwrap();
    let note_id = item.id;

    // 3. Initial note fetch (revision should be 0)
    let get_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/bookmarks/{}/note", note_id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let get_resp = app.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let note_bytes = axum::body::to_bytes(get_resp.into_body(), 1024 * 64).await.unwrap();
    let note_data: NoteResponse = serde_json::from_slice(&note_bytes).unwrap();
    assert_eq!(note_data.revision, 0);

    // 4. Client A updates note based on revision 0
    let save_id_1 = Uuid::now_v7();
    let update_req_1 = Request::builder()
        .method("PUT")
        .uri(format!("/v1/bookmarks/{}/note", note_id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateNoteRequest {
                base_revision: 0,
                save_id: Some(save_id_1),
                document: Some(serde_json::json!({ "type": "paragraph", "text": "Draft content" })),
                plain_text: Some("Draft content discussing modular monolith architecture".to_string()),
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();

    let update_resp_1 = app.clone().oneshot(update_req_1).await.unwrap();
    assert_eq!(update_resp_1.status(), StatusCode::OK);
    let u1_bytes = axum::body::to_bytes(update_resp_1.into_body(), 1024 * 64).await.unwrap();
    let u1_data: NoteResponse = serde_json::from_slice(&u1_bytes).unwrap();
    assert_eq!(u1_data.revision, 1);
    assert_eq!(u1_data.save_id, Some(save_id_1));

    // 5. Verify Postgres search_vector was populated with the plain_text via trigger!
    let search_vector: String = sqlx::query_scalar(
        "SELECT search_vector::text FROM bookmarks WHERE id = $1",
    )
    .bind(note_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(search_vector.contains("'monolith':"));
    assert!(search_vector.contains("'architecture':"));

    // 6. Test Idempotent Retry (Client A retries with same save_id_1)
    let retry_req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/bookmarks/{}/note", note_id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateNoteRequest {
                base_revision: 0,
                save_id: Some(save_id_1),
                document: Some(serde_json::json!({ "type": "paragraph", "text": "Draft content" })),
                plain_text: Some("Draft content discussing modular monolith architecture".to_string()),
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();

    let retry_resp = app.clone().oneshot(retry_req).await.unwrap();
    assert_eq!(retry_resp.status(), StatusCode::OK);
    let retry_bytes = axum::body::to_bytes(retry_resp.into_body(), 1024 * 64).await.unwrap();
    let retry_data: NoteResponse = serde_json::from_slice(&retry_bytes).unwrap();
    assert_eq!(retry_data.revision, 1);

    // 7. Client B attempts update based on stale base_revision = 0 (Conflict!)
    let save_id_2 = Uuid::now_v7();
    let conflict_req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/bookmarks/{}/note", note_id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateNoteRequest {
                base_revision: 0, // Stale! Current revision is 1!
                save_id: Some(save_id_2),
                document: Some(serde_json::json!({ "type": "paragraph", "text": "Conflicting edit" })),
                plain_text: Some("Conflicting edit from other tab".to_string()),
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();

    let conflict_resp = app.clone().oneshot(conflict_req).await.unwrap();
    assert_eq!(conflict_resp.status(), StatusCode::CONFLICT);

    // 8. Client B resolves conflict by updating with base_revision = 1
    let resolve_req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/bookmarks/{}/note", note_id))
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateNoteRequest {
                base_revision: 1, // Correct!
                save_id: Some(save_id_2),
                document: Some(serde_json::json!({ "type": "paragraph", "text": "Resolved edit" })),
                plain_text: Some("Resolved edit merged with current".to_string()),
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();

    let resolve_resp = app.clone().oneshot(resolve_req).await.unwrap();
    assert_eq!(resolve_resp.status(), StatusCode::OK);
    let resolve_bytes = axum::body::to_bytes(resolve_resp.into_body(), 1024 * 64).await.unwrap();
    let resolve_data: NoteResponse = serde_json::from_slice(&resolve_bytes).unwrap();
    assert_eq!(resolve_data.revision, 2);
}
