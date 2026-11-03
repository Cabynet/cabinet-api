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
        tasks::{
            handlers::router as tasks_router,
            parser::NetscapeParser,
            service::TasksService,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_html_import_with_nested_folders_and_export_roundtrip() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .nest("/v1/tasks", tasks_router())
        .with_state(pool.clone());

    // 1. Register test user
    let email = format!("import_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Importer User".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Sample Netscape HTML content with nested folders
    let sample_html = r#"
        <!DOCTYPE NETSCAPE-Bookmark-file-1>
        <META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
        <TITLE>Bookmarks</TITLE>
        <H1>Bookmarks</H1>
        <DL><p>
            <DT><A HREF="https://root.example.com" ADD_DATE="1609459200">Root Link</A>
            <DT><H3 ADD_DATE="1609459201">Engineering</H3>
            <DL><p>
                <DT><A HREF="https://eng.example.com" ADD_DATE="1609459202">Engineering Link</A>
                <DT><H3 ADD_DATE="1609459203">Backend Systems</H3>
                <DL><p>
                    <DT><A HREF="https://rust-lang.org/learn" ADD_DATE="1609459204">Rust Guide</A>
                </DL><p>
            </DL><p>
        </DL><p>
    "#;

    // 3. Test TasksService Import
    let tasks_service = TasksService::new(pool.clone(), None);
    let mut task_dto = tasks_service
        .import_html(session.user.id, sample_html)
        .await
        .unwrap();

    assert_eq!(task_dto.status, "PENDING");
    for _ in 0..500 {
        task_dto = tasks_service.get_task_status(session.user.id, task_dto.id).await.unwrap();
        if matches!(task_dto.status.as_str(), "COMPLETED" | "FAILED") { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(task_dto.kind, "IMPORT_HTML");
    assert_eq!(task_dto.status, "COMPLETED");
    assert_eq!(task_dto.total_items, 3);
    assert_eq!(task_dto.processed_items, 3);
    assert_eq!(task_dto.failed_items, 0);

    // 4. Verify Task can be queried via GET /v1/tasks/:id
    let get_task_req = Request::builder()
        .method("GET")
        .uri(format!("/v1/tasks/{}", task_dto.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let get_task_resp = app.clone().oneshot(get_task_req).await.unwrap();
    assert_eq!(get_task_resp.status(), StatusCode::OK);

    // 5. Verify database records
    let bookmark_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM bookmarks WHERE user_id = $1 AND deleted_at IS NULL",
    )
    .bind(session.user.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bookmark_count, 3);

    let folder_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM folders WHERE user_id = $1",
    )
    .bind(session.user.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(folder_count, 2); // "Engineering" and child "Backend Systems"

    // 6. Test Export via GET /v1/bookmarks/export
    let export_req = Request::builder()
        .method("GET")
        .uri("/v1/bookmarks/export")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let export_resp = app.clone().oneshot(export_req).await.unwrap();
    assert_eq!(export_resp.status(), StatusCode::OK);

    let content_type = export_resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/html"));

    let export_bytes = axum::body::to_bytes(export_resp.into_body(), 1024 * 64).await.unwrap();
    let export_html = String::from_utf8(export_bytes.to_vec()).unwrap();

    assert!(export_html.contains("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
    assert!(export_html.contains("Engineering"));
    assert!(export_html.contains("Backend Systems"));
    assert!(export_html.contains("https://rust-lang.org/learn"));

    // 7. Verify Round-trip: Re-parsing exported HTML recovers the entire tree!
    let reparsed = NetscapeParser::parse(&export_html);
    assert_eq!(reparsed.folders.len(), 2);
    assert_eq!(reparsed.bookmarks.len(), 3);
}
