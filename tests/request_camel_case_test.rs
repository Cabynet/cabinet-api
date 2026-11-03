// Request bodies use camelCase keys; serde ignores unknown keys, so a missing alias would drop a field silently.
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
async fn camel_case_request_keys_are_honored() {
    let pool = create_db_pool(&common::config()).await.unwrap();
    let app = Router::new()
        .nest("/v1/auth", modules::auth::handlers::router())
        .nest("/v1/bookmarks", modules::bookmarks::handlers::router())
        .nest("/v1/folders", modules::folders::handlers::router())
        .nest("/v1/media", modules::media::handlers::router())
        .with_state(pool);
    let user = call(&app, "POST", "/v1/auth/register", None, json!({"email": format!("camel-{}@example.com", Uuid::now_v7()), "name": "Camel", "password": "CamelCase!12345"}), 201).await;
    let token = user["token"].as_str();

    // create: itemType + clientRequestId + notesPlain, then contentType on a link
    let note = call(&app, "POST", "/v1/bookmarks", token, json!({"title": "N", "itemType": "NOTE", "clientRequestId": Uuid::now_v7(), "notesPlain": "hello"}), 201).await;
    assert_eq!(note["itemType"], "NOTE");
    assert_eq!(note["notesPlain"], "hello");
    let pdf = call(&app, "POST", "/v1/bookmarks", token, json!({"url": "https://example.com/a.pdf", "title": "P", "contentType": "PDF"}), 201).await;
    assert_eq!(pdf["contentType"], "PDF");
    let id = note["id"].as_str().unwrap();

    // note PUT: baseRevision/saveId give revision bump, idempotent retry, and a 409 on a stale revision
    let note_path = format!("/v1/bookmarks/{id}/note");
    let save_id = Uuid::now_v7();
    let body = json!({"baseRevision": 0, "saveId": save_id, "document": {"root": {}}, "plainText": "first"});
    let first = call(&app, "PUT", &note_path, token, body.clone(), 200).await;
    assert_eq!(first["revision"], 1);
    let retry = call(&app, "PUT", &note_path, token, body, 200).await;
    assert_eq!(retry["revision"], 1, "same saveId must be idempotent");
    call(&app, "PUT", &note_path, token, json!({"baseRevision": 0, "saveId": Uuid::now_v7(), "plainText": "stale"}), 409).await;

    // update bookmark: a stale baseRevision on notesPlain must conflict (it used to be dropped silently)
    let stale = json!({"notesPlain": "x", "baseRevision": 0, "saveId": Uuid::now_v7()});
    call(&app, "PATCH", &format!("/v1/bookmarks/{id}"), token, stale, 409).await;
    let fresh = json!({"notesPlain": "second", "baseRevision": 1, "saveId": Uuid::now_v7(), "isPinned": true});
    let updated = call(&app, "PATCH", &format!("/v1/bookmarks/{id}"), token, fresh, 200).await;
    assert_eq!(updated["isPinned"], true);

    // presign: mimeType + sizeBytes
    let presign = call(&app, "POST", "/v1/media/presign", token, json!({"filename": "a.png", "mimeType": "image/png", "sizeBytes": 1000}), 201).await;
    assert!(presign["uploadUrl"].is_string());

    // folder update: parentId null moves to root
    let parent = call(&app, "POST", "/v1/folders", token, json!({"name": "Parent"}), 201).await;
    let child = call(&app, "POST", "/v1/folders", token, json!({"name": "Child", "parentId": parent["id"]}), 201).await;
    assert_eq!(child["parentId"], parent["id"]);
    let moved = call(&app, "PUT", &format!("/v1/folders/{}", child["id"].as_str().unwrap()), token, json!({"parentId": null}), 200).await;
    assert!(moved["parentId"].is_null());
}

#[tokio::test]
async fn snake_case_aliases_still_work() {
    let pool = create_db_pool(&common::config()).await.unwrap();
    let app = Router::new()
        .nest("/v1/auth", modules::auth::handlers::router())
        .nest("/v1/bookmarks", modules::bookmarks::handlers::router())
        .with_state(pool);
    let user = call(&app, "POST", "/v1/auth/register", None, json!({"email": format!("snake-{}@example.com", Uuid::now_v7()), "name": "Snake", "password": "SnakeCase!12345"}), 201).await;
    let note = call(&app, "POST", "/v1/bookmarks", user["token"].as_str(), json!({"title": "N", "item_type": "NOTE", "notes_plain": "old"}), 201).await;
    assert_eq!(note["itemType"], "NOTE");
    assert_eq!(note["notesPlain"], "old");
}

#[tokio::test]
async fn proxy_page_requires_a_page_signature() {
    let pool = create_db_pool(&common::config()).await.unwrap();
    let app = Router::new()
        .nest("/v1/auth", modules::auth::handlers::router())
        .nest("/v1/bookmarks", modules::bookmarks::handlers::router())
        .with_state(pool);
    let user = call(&app, "POST", "/v1/auth/register", None, json!({"email": format!("page-{}@example.com", Uuid::now_v7()), "name": "Pager", "password": "PageSign!12345"}), 201).await;
    let token = user["token"].as_str();
    let url = "https://example.com/article";
    let enc = "https%3A%2F%2Fexample.com%2Farticle";

    // no signature, and an image signature, are both refused
    let status = |path: String| { let app = app.clone(); async move { app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap().status().as_u16() } };
    assert_eq!(status(format!("/v1/bookmarks/proxy-page?url={enc}")).await, 403);
    let image = call(&app, "POST", "/v1/bookmarks/proxy-image/sign", token, json!({"urls": [url]}), 200).await;
    let image_url = image["items"][0]["signedUrl"].as_str().unwrap();
    assert!(image_url.starts_with("/v1/bookmarks/proxy-image?"));
    let image_sig = image_url.rsplit("sig=").next().unwrap();
    assert_eq!(status(format!("/v1/bookmarks/proxy-page?url={enc}&sig={image_sig}")).await, 403);

    // kind: "page" signs for /proxy-page; a tampered URL no longer verifies
    let page = call(&app, "POST", "/v1/bookmarks/proxy-image/sign", token, json!({"urls": [url], "kind": "page"}), 200).await;
    let page_url = page["items"][0]["signedUrl"].as_str().unwrap();
    assert!(page_url.starts_with("/v1/bookmarks/proxy-page?"));
    let page_sig = page_url.rsplit("sig=").next().unwrap();
    assert_eq!(status(format!("/v1/bookmarks/proxy-page?url=https%3A%2F%2Fexample.com%2Fother&sig={page_sig}")).await, 403);
    call(&app, "POST", "/v1/bookmarks/proxy-image/sign", token, json!({"urls": [url], "kind": "nope"}), 400).await;
}
