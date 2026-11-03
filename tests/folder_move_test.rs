mod common;
use axum::{body::{to_bytes, Body}, http::Request, Router};
use cabinet_api::{infra::db::create_db_pool, modules};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(app: &Router, method: &str, path: &str, token: Option<&str>, body: Value, status: u16) -> Value {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token { request = request.header("Authorization", format!("Bearer {token}")); }
    let request = if body.is_null() { request.body(Body::empty()) } else { request.header("Content-Type", "application/json").body(Body::from(body.to_string())) }.unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let actual = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap();
    assert_eq!(actual, status, "{method} {path}: {}", String::from_utf8_lossy(&bytes));
    if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() }
}

#[tokio::test]
async fn folder_can_move_to_root_and_clear_description() {
    let pool = create_db_pool(&common::config()).await.unwrap();
    let app = Router::new().nest("/v1/auth", modules::auth::handlers::router()).nest("/v1/folders", modules::folders::handlers::router()).with_state(pool);
    let user = call(&app, "POST", "/v1/auth/register", None, json!({"email": format!("move-{}@example.com", Uuid::now_v7()), "name": "Mover", "password": "MoveOnly!12345"}), 201).await;
    let token = user["token"].as_str();
    let parent = call(&app, "POST", "/v1/folders", token, json!({"name": "Parent"}), 201).await;
    let child = call(&app, "POST", "/v1/folders", token, json!({"name": "Child", "description": "keep", "parent_id": parent["id"]}), 201).await;
    let path = format!("/v1/folders/{}", child["id"].as_str().unwrap());

    // Absent fields leave parent and description untouched.
    let same = call(&app, "PUT", &path, token, json!({"name": "Child 2"}), 200).await;
    assert_eq!(same["parentId"], parent["id"]);
    assert_eq!(same["description"], "keep");

    // Explicit null moves to root and clears the description.
    let moved = call(&app, "PUT", &path, token, json!({"parent_id": null, "description": null}), 200).await;
    assert!(moved["parentId"].is_null());
    assert!(moved["description"].is_null());
}
