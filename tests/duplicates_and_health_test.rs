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
            dto::{
                CreateBookmarkRequest, DuplicateCleanupResponse, DuplicateReportResponse,
                LinkHealthCheckResponse,
            },
            handlers::router as bookmarks_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_duplicates_detection_cleanup_and_health_check() {
    let mut config = AppConfig::from_env().expect("config");
    config.database_url = "postgresql://postgres:postgrespassword@localhost:5432/cabinet".to_string();

    let pool = match create_db_pool(&config).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .with_state(pool.clone());

    // 1. Register test user
    let email = format!("dups_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email,
                password: "Password123!".to_string(),
                name: "Dups Tester".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    let body_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Insert 3 duplicate links with different tracking parameters
    let urls = [
        "https://example.com/target-article?utm_source=twitter",
        "https://example.com/target-article?ref=newsletter",
        "https://example.com/target-article/",
    ];

    let mut created_ids = Vec::new();
    for (i, u) in urls.iter().enumerate() {
        let bm_req = Request::builder()
            .method("POST")
            .uri("/v1/bookmarks")
            .header(header::AUTHORIZATION, &auth_header)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&CreateBookmarkRequest {
                    url: Some(u.to_string()),
                    title: Some(format!("Target Article Version {}", i + 1)),
                    description: None,
                    folder_id: None,
                    item_type: Some("LINK".to_string()),
                    client_request_id: Some(Uuid::now_v7()),
                    tags: None,
                })
                .unwrap(),
            ))
            .unwrap();

        let bm_resp = app.clone().oneshot(bm_req).await.unwrap();
        assert_eq!(bm_resp.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(bm_resp.into_body(), 1024 * 64).await.unwrap();
        let item: cabinet_api::modules::bookmarks::dto::BookmarkListItem =
            serde_json::from_slice(&bytes).unwrap();
        created_ids.push(item.id);
    }

    // 3. Query Duplicates via GET /v1/bookmarks/duplicates
    let dups_req = Request::builder()
        .method("GET")
        .uri("/v1/bookmarks/duplicates")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let dups_resp = app.clone().oneshot(dups_req).await.unwrap();
    assert_eq!(dups_resp.status(), StatusCode::OK);
    let dups_bytes = axum::body::to_bytes(dups_resp.into_body(), 1024 * 64).await.unwrap();
    let dups_report: DuplicateReportResponse = serde_json::from_slice(&dups_bytes).unwrap();

    assert_eq!(dups_report.total_duplicate_groups, 1);
    assert_eq!(dups_report.total_redundant_items, 2);
    assert_eq!(dups_report.groups[0].count, 3);

    // 4. Cleanup Duplicates via POST /v1/bookmarks/duplicates/cleanup
    let cleanup_req = Request::builder()
        .method("POST")
        .uri("/v1/bookmarks/duplicates/cleanup")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let cleanup_resp = app.clone().oneshot(cleanup_req).await.unwrap();
    assert_eq!(cleanup_resp.status(), StatusCode::OK);
    let cleanup_bytes = axum::body::to_bytes(cleanup_resp.into_body(), 1024 * 64).await.unwrap();
    let cleanup_data: DuplicateCleanupResponse = serde_json::from_slice(&cleanup_bytes).unwrap();

    assert_eq!(cleanup_data.cleaned_items, 2);

    // Verify earliest bookmark (created_ids[0]) is still active, but created_ids[1] & [2] are soft-deleted
    let active_first: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM bookmarks WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(created_ids[0])
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(active_first, Some(created_ids[0]));

    let deleted_second: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM bookmarks WHERE id = $1 AND deleted_at IS NOT NULL",
    )
    .bind(created_ids[1])
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(deleted_second, Some(created_ids[1]));

    // 5. Verify duplicates report is now 0
    let dups_req_after = Request::builder()
        .method("GET")
        .uri("/v1/bookmarks/duplicates")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let dups_resp_after = app.clone().oneshot(dups_req_after).await.unwrap();
    let dups_bytes_after = axum::body::to_bytes(dups_resp_after.into_body(), 1024 * 64).await.unwrap();
    let dups_report_after: DuplicateReportResponse = serde_json::from_slice(&dups_bytes_after).unwrap();
    assert_eq!(dups_report_after.total_duplicate_groups, 0);

    // 6. Test Link Health Checker on the active bookmark
    let check_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/bookmarks/{}/check-link", created_ids[0]))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let check_resp = app.clone().oneshot(check_req).await.unwrap();
    assert_eq!(check_resp.status(), StatusCode::OK);
    let check_bytes = axum::body::to_bytes(check_resp.into_body(), 1024 * 64).await.unwrap();
    let check_data: LinkHealthCheckResponse = serde_json::from_slice(&check_bytes).unwrap();

    assert_eq!(check_data.id, created_ids[0]);
    assert!(check_data.link_health == "OK" || check_data.link_health == "REDIRECT" || check_data.link_health == "BROKEN");

    // Verify DB columns link_health and last_checked_at were updated
    let health_db: (String, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "SELECT link_health, last_checked_at FROM bookmarks WHERE id = $1",
    )
    .bind(created_ids[0])
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(health_db.0, check_data.link_health);
    assert!(health_db.1.is_some());
}
