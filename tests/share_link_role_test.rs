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
use serde_json::{json, Value};

fn member_role(collaboration: &Value) -> Option<String> {
    collaboration["members"].as_array().unwrap().first().map(|m| m["role"].as_str().unwrap().to_string())
}

/// One link per folder, like Google Drive: its role decides whether visitors only view or can join.
#[tokio::test]
async fn folder_link_role_controls_joining() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");
    let app = Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/folders", folders_router())
        .nest("/v1/share", public_share_router())
        .with_state(pool.clone());

    let owner = register(&app, "link_owner").await;
    let guest = register(&app, "link_guest").await;
    let (_, folder) = call(&app, "POST", "/v1/folders", Some(&owner), Some(json!({ "name": "Team" }))).await;
    let id = folder["id"].as_str().unwrap().to_string();
    let share = format!("/v1/folders/{id}/share");
    let collaboration = format!("/v1/folders/{id}/collaboration");

    // A link without a role is view-only, as before.
    let (_, link) = call(&app, "POST", &share, Some(&owner), None).await;
    assert_eq!(link["role"], "VIEWER");
    let token = link["shareToken"].as_str().unwrap().to_string();
    let (_, public) = call(&app, "GET", &format!("/v1/share/{token}"), None, None).await;
    assert_eq!(public["role"], "VIEWER");

    // Joining needs a signed-in visitor; a view link adds them as a viewer ("Shared with me").
    let join = format!("/v1/share/{token}/join");
    let (status, _) = call(&app, "POST", &join, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, joined) = call(&app, "POST", &join, Some(&guest), None).await;
    assert_eq!(status, StatusCode::OK, "{joined}");
    assert_eq!(joined["folderId"], id.as_str());
    let (_, info) = call(&app, "GET", &collaboration, Some(&owner), None).await;
    assert_eq!(member_role(&info).as_deref(), Some("VIEWER"));

    // Raising the link's role keeps the same link and upgrades people who join with it.
    let (status, link) = call(&app, "PATCH", &share, Some(&owner), Some(json!({ "role": "EDITOR" }))).await;
    assert_eq!(status, StatusCode::OK, "{link}");
    assert_eq!(link["shareToken"], token.as_str());
    assert_eq!(link["role"], "EDITOR");
    let (_, status_body) = call(&app, "GET", &share, Some(&owner), None).await;
    assert_eq!(status_body["role"], "EDITOR");
    let (_, public) = call(&app, "GET", &format!("/v1/share/{token}"), None, None).await;
    assert_eq!(public["role"], "EDITOR");
    call(&app, "POST", &join, Some(&guest), None).await;
    let (_, info) = call(&app, "GET", &collaboration, Some(&owner), None).await;
    assert_eq!(member_role(&info).as_deref(), Some("EDITOR"));

    // Lowering the link never downgrades someone who already has more access.
    call(&app, "PATCH", &share, Some(&owner), Some(json!({ "role": "VIEWER" }))).await;
    call(&app, "POST", &join, Some(&guest), None).await;
    let (_, info) = call(&app, "GET", &collaboration, Some(&owner), None).await;
    assert_eq!(member_role(&info).as_deref(), Some("EDITOR"));

    // Changing only the expiry keeps the role; invalid roles and empty updates are rejected.
    call(&app, "PATCH", &share, Some(&owner), Some(json!({ "role": "CONTRIBUTOR" }))).await;
    let (_, link) = call(&app, "PATCH", &share, Some(&owner), Some(json!({ "expiresInDays": 7 }))).await;
    assert_eq!(link["role"], "CONTRIBUTOR");
    assert!(link["expiresAt"].is_string());
    for body in [json!({ "role": "OWNER" }), json!({})] {
        let (status, _) = call(&app, "PATCH", &share, Some(&owner), Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = call(&app, "POST", &share, Some(&owner), Some(json!({ "role": "OWNER" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The owner opening their own link is not added as a member.
    call(&app, "DELETE", &format!("/v1/folders/{id}/members/{}", info["members"][0]["userId"].as_str().unwrap()), Some(&owner), None).await;
    let (status, _) = call(&app, "POST", &join, Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, info) = call(&app, "GET", &collaboration, Some(&owner), None).await;
    assert_eq!(member_role(&info), None);

    // A new link can start with a role; turned-off and expired links admit no one.
    let (_, link) = call(&app, "POST", &share, Some(&owner), Some(json!({ "role": "EDITOR" }))).await;
    assert_eq!(link["role"], "EDITOR");
    let fresh = link["shareToken"].as_str().unwrap().to_string();
    let (status, _) = call(&app, "POST", &join, Some(&guest), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the old link stopped working when a new one was made");
    sqlx::query("UPDATE folders SET share_expires_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(&id).execute(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", &format!("/v1/share/{fresh}/join"), Some(&guest), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, info) = call(&app, "GET", &collaboration, Some(&owner), None).await;
    assert_eq!(member_role(&info), None);
}
