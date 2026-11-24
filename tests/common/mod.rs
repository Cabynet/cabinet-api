use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use cabinet_api::{config::AppConfig, modules::auth::dto::{RegisterRequest, SessionResponse}};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

pub fn config() -> AppConfig {
    let database = std::env::var("TEST_DATABASE_URL").expect("Set TEST_DATABASE_URL to an isolated *_test database; use scripts/test.sh");
    let redis = std::env::var("TEST_REDIS_URL").expect("Set TEST_REDIS_URL to an isolated Redis instance; use scripts/test.sh");
    assert_eq!(std::env::var("DATABASE_URL").ok().as_deref(), Some(database.as_str()), "Handlers and tests must use the same isolated database");
    assert_eq!(std::env::var("REDIS_URL").ok().as_deref(), Some(redis.as_str()), "Handlers and tests must use the same isolated Redis");
    let url = url::Url::parse(&database).expect("Test database URL");
    assert!(url.path().contains("_test"), "Refusing a database without _test in its name");
    AppConfig::from_env().expect("Test configuration")
}

// JSON request helpers for router-level tests (not every test file uses them).
#[allow(dead_code)]
pub async fn call(app: &Router, method: &str, uri: &str, auth: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    let body = match body {
        Some(body) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[allow(dead_code)]
pub async fn register(app: &Router, name: &str) -> String {
    let (status, body) = call(app, "POST", "/v1/auth/register", None, Some(serde_json::to_value(RegisterRequest {
        email: format!("{name}_{}@example.com", Uuid::now_v7()),
        password: "Password123!".to_string(),
        name: name.to_string(),
    }).unwrap())).await;
    assert!(status.is_success(), "register failed: {status} {body}");
    let session: SessionResponse = serde_json::from_value(body).unwrap();
    format!("Bearer {}", session.token)
}
