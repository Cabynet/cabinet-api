use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
mod common;
use cabinet_api::{
    infra::{db::create_db_pool, proxy_sign},
    modules::auth::{dto::RegisterRequest, service::AuthService},
    modules::bookmarks::handlers::router as bookmarks_router,
};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

async fn json(resp: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1024 * 256).await.unwrap()).unwrap()
}

fn post_sign(auth: Option<&str>, body: serde_json::Value) -> Request<Body> {
    let mut req = Request::builder().method("POST").uri("/v1/bookmarks/proxy-image/sign").header(header::CONTENT_TYPE, "application/json");
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    req.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn proxy_only_serves_signed_urls_and_signing_is_scoped() {
    std::env::set_var("PROXY_SIGNING_KEY", "test-proxy-key");
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");
    let app = axum::Router::new().nest("/v1/bookmarks", bookmarks_router()).with_state(pool.clone());

    // 1. Unsigned or wrongly signed proxy requests are refused before any fetch happens.
    for uri in [
        "/v1/bookmarks/proxy-image?url=https%3A%2F%2Fexample.com%2Fa.png",
        "/v1/bookmarks/proxy-image?url=https%3A%2F%2Fexample.com%2Fa.png&sig=deadbeef",
    ] {
        let resp = app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }

    // 2. Anonymous callers cannot mint signatures without a share token.
    let resp = app.clone().oneshot(post_sign(None, serde_json::json!({ "urls": ["https://example.com/a.png"] }))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 3. Signed-in users get verifiable signed URLs.
    let session = AuthService::new(pool.clone())
        .register(
            RegisterRequest { email: format!("sign_{}@example.com", Uuid::now_v7()), password: "Password123!".into(), name: "Signer".into() },
            "WEB", None, None,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", session.token);
    let resp = app.clone().oneshot(post_sign(Some(&bearer), serde_json::json!({ "urls": ["https://example.com/a b.png"] }))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let item = &body["items"][0];
    assert_eq!(item["url"], "https://example.com/a b.png");
    assert!(item["signedUrl"].as_str().unwrap().contains(&format!("sig={}", proxy_sign::sign("https://example.com/a b.png"))));

    // 4. A share token only signs URLs of items in that shared folder.
    let (folder, item_id) = (Uuid::now_v7(), Uuid::now_v7());
    let token = "a".repeat(64);
    sqlx::query("INSERT INTO folders (id, user_id, name, share_token_hash) VALUES ($1, $2, 'Shared', $3)")
        .bind(folder)
        .bind(session.user.id)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO bookmarks (id, user_id, folder_id, item_type, raw_url, title, thumbnail_url) VALUES ($1, $2, $3, 'LINK', 'https://shared.example/x', 't', 'https://shared.example/thumb.png')")
        .bind(item_id)
        .bind(session.user.id)
        .bind(folder)
        .execute(&pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(post_sign(None, serde_json::json!({
            "shareToken": token,
            "urls": ["https://shared.example/thumb.png", "https://internal.example/secret"],
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let urls: Vec<_> = body["items"].as_array().unwrap().iter().map(|i| i["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["https://shared.example/thumb.png"]);

    let resp = app.clone().oneshot(post_sign(None, serde_json::json!({ "shareToken": "b".repeat(64), "urls": [] }))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
