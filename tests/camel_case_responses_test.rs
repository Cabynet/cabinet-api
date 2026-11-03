mod common;
use axum::{body::{to_bytes, Body}, http::Request, Router};
use cabinet_api::{infra::db::create_db_pool, modules};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const USER_DOCS: [&str; 3] = ["noteDocument", "document", "drawing"];

async fn call(app: &Router, method: &str, path: &str, token: Option<&str>, body: Value) -> Value {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let req = if body.is_null() { req.body(Body::empty()) } else { req.header("Content-Type", "application/json").body(Body::from(body.to_string())) }.unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    serde_json::from_slice(&to_bytes(res.into_body(), 4 * 1024 * 1024).await.unwrap()).unwrap()
}

fn assert_camel(v: &Value, path: &str) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                assert!(!k.contains('_'), "snake_case key `{k}` at {path}");
                assert!(k.chars().next().is_none_or(|c| c.is_ascii_lowercase()), "non-camelCase key `{k}` at {path}");
                if !USER_DOCS.contains(&k.as_str()) {
                    assert_camel(v, &format!("{path}.{k}"));
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| assert_camel(i, path)),
        _ => {}
    }
}

#[tokio::test]
async fn every_response_uses_camel_case_keys_and_user_documents_are_untouched() {
    let pool = create_db_pool(&common::config()).await.unwrap();
    let app = Router::new()
        .nest("/v1/auth", modules::auth::handlers::router())
        .nest("/v1/bookmarks", modules::bookmarks::handlers::router())
        .nest("/v1/folders", modules::folders::handlers::router())
        .nest("/v1/tags", modules::tags::handlers::router())
        .nest("/v1/activity", modules::activity::router())
        .with_state(pool);

    let session = call(&app, "POST", "/v1/auth/register", None, json!({"email": format!("camel-{}@example.com", Uuid::now_v7()), "name": "Camel test", "password": "CamelOnly!123"})).await;
    assert_camel(&session, "register");
    let t = session["token"].as_str();

    let folder = call(&app, "POST", "/v1/folders", t, json!({"name": "Camel folder"})).await;
    let note = call(&app, "POST", "/v1/bookmarks", t, json!({"title": "Note", "item_type": "NOTE", "folder_id": folder["id"], "tags": ["camel"]})).await;
    let id = note["id"].as_str().unwrap();
    let saved = call(&app, "PUT", &format!("/v1/bookmarks/{id}/note"), t, json!({"base_revision": 0, "save_id": Uuid::now_v7(), "document": {"type": "doc", "some_key": 1}, "plain_text": "x"})).await;
    assert_eq!(saved["document"]["some_key"], 1, "user document must pass through untouched");

    for path in ["/v1/bookmarks", "/v1/bookmarks?summary=true", &format!("/v1/bookmarks/{id}"), "/v1/folders", "/v1/tags", "/v1/auth/me", "/v1/activity", &format!("/v1/folders/{}/share", folder["id"].as_str().unwrap()), &format!("/v1/bookmarks/{}", Uuid::now_v7())] {
        let body = call(&app, "GET", path, t, Value::Null).await;
        assert_camel(&body, path);
    }
    let list = call(&app, "GET", "/v1/bookmarks", t, Value::Null).await;
    assert!(list["items"][0]["itemType"].is_string() && list["totalCount"].is_number());
    let fetched = call(&app, "GET", &format!("/v1/bookmarks/{id}"), t, Value::Null).await;
    assert_eq!(fetched["noteDocument"]["some_key"], 1);
}
