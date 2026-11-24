use axum::{http::StatusCode, Router};
mod common;
use cabinet_api::{
    infra::db::create_db_pool,
    modules::{
        auth::handlers::router as auth_router,
        folders::handlers::{public_share_router, router as folders_router},
    },
};
use common::{call, register};
use serde_json::json;

#[tokio::test]
async fn share_links_expire() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");
    let app = Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/folders", folders_router())
        .nest("/v1/share", public_share_router())
        .with_state(pool.clone());

    let owner = register(&app, "expiry_owner").await;
    let guest = register(&app, "expiry_guest").await;
    let (status, folder) = call(&app, "POST", "/v1/folders", Some(&owner), Some(json!({ "name": "Expiring" }))).await;
    assert!(status.is_success(), "{status} {folder}");
    let id = folder["id"].as_str().unwrap().to_string();

    // Public link: a new link never expires, and its expiry can be set and cleared.
    let (_, share) = call(&app, "POST", &format!("/v1/folders/{id}/share"), Some(&owner), None).await;
    let token = share["shareToken"].as_str().unwrap().to_string();
    assert!(share["expiresAt"].is_null());
    let (status, body) = call(&app, "PATCH", &format!("/v1/folders/{id}/share"), Some(&owner), Some(json!({ "expiresInDays": 7 }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["shareToken"], token);
    assert!(body["expiresAt"].is_string());
    let (_, status_body) = call(&app, "GET", &format!("/v1/folders/{id}/share"), Some(&owner), None).await;
    assert_eq!(status_body["expiresAt"], body["expiresAt"]);
    for invalid in [0, 366] {
        let (status, _) = call(&app, "PATCH", &format!("/v1/folders/{id}/share"), Some(&owner), Some(json!({ "expiresInDays": invalid }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (_, cleared) = call(&app, "PATCH", &format!("/v1/folders/{id}/share"), Some(&owner), Some(json!({ "expiresInDays": null }))).await;
    assert!(cleared["expiresAt"].is_null());

    // Once expired, the public link is gone everywhere and cannot be revived by editing it.
    sqlx::query("UPDATE folders SET share_expires_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(&id).execute(&pool).await.unwrap();
    let (status, _) = call(&app, "GET", &format!("/v1/share/{token}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "POST", &format!("/v1/share/{token}/copy"), Some(&guest), Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, status_body) = call(&app, "GET", &format!("/v1/folders/{id}/share"), Some(&owner), None).await;
    assert!(status_body["shareToken"].is_null());
    let (status, _) = call(&app, "PATCH", &format!("/v1/folders/{id}/share"), Some(&owner), Some(json!({ "expiresInDays": 7 }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, folders) = call(&app, "GET", "/v1/folders", Some(&owner), None).await;
    let listed = folders.as_array().unwrap().iter().find(|f| f["id"] == id.as_str()).unwrap();
    assert_eq!(listed["shareEnabled"], false);
    // Turning sharing on again makes a fresh link without the old expiry.
    let (_, share) = call(&app, "POST", &format!("/v1/folders/{id}/share"), Some(&owner), None).await;
    let fresh = share["shareToken"].as_str().unwrap();
    let (status, _) = call(&app, "GET", &format!("/v1/share/{fresh}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
}
