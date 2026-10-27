use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use cabinet_api::{
    config::AppConfig,
    infra::{
        rate_limit::{
            rate_limit_middleware, RateLimitConfig, X_RATELIMIT_LIMIT, X_RATELIMIT_REMAINING,
            X_RATELIMIT_RESET,
        },
        redis::create_redis_pool,
    },
};
use tower::ServiceExt;

#[tokio::test]
async fn test_redis_token_bucket_rate_limiter_returns_429() {
    let config = AppConfig::from_env().expect("config");
    let redis = match create_redis_pool(&config).await {
        Ok(r) => r,
        Err(_) => {
            eprintln!("Skipping rate limiter test: Redis not reachable");
            return;
        }
    };

    let test_config = RateLimitConfig {
        prefix: "unit_test_auth",
        max_requests: 3,
        window_seconds: 5,
    };

    let redis_for_mw = redis.clone();
    let app = Router::new()
        .route(
            "/test-limited",
            get(|| async { (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response() }),
        )
        .layer(middleware::from_fn(move |req, next| {
            rate_limit_middleware(redis_for_mw.clone(), test_config, req, next)
        }));

    let client_ip = "203.0.113.88";

    // Request 1: Allowed (Remaining = 2)
    let req1 = Request::builder()
        .method("GET")
        .uri("/test-limited")
        .header("x-forwarded-for", client_ip)
        .body(Body::empty())
        .unwrap();

    let resp1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    assert_eq!(
        resp1.headers().get(X_RATELIMIT_LIMIT).unwrap().to_str().unwrap(),
        "3"
    );
    assert_eq!(
        resp1.headers().get(X_RATELIMIT_REMAINING).unwrap().to_str().unwrap(),
        "2"
    );

    // Request 2: Allowed (Remaining = 1)
    let req2 = Request::builder()
        .method("GET")
        .uri("/test-limited")
        .header("x-forwarded-for", client_ip)
        .body(Body::empty())
        .unwrap();

    let resp2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    assert_eq!(
        resp2.headers().get(X_RATELIMIT_REMAINING).unwrap().to_str().unwrap(),
        "1"
    );

    // Request 3: Allowed (Remaining = 0)
    let req3 = Request::builder()
        .method("GET")
        .uri("/test-limited")
        .header("x-forwarded-for", client_ip)
        .body(Body::empty())
        .unwrap();

    let resp3 = app.clone().oneshot(req3).await.unwrap();
    assert_eq!(resp3.status(), StatusCode::OK);
    assert_eq!(
        resp3.headers().get(X_RATELIMIT_REMAINING).unwrap().to_str().unwrap(),
        "0"
    );

    // Request 4: Blocked! 429 Too Many Requests
    let req4 = Request::builder()
        .method("GET")
        .uri("/test-limited")
        .header("x-forwarded-for", client_ip)
        .body(Body::empty())
        .unwrap();

    let resp4 = app.clone().oneshot(req4).await.unwrap();
    assert_eq!(resp4.status(), StatusCode::TOO_MANY_REQUESTS);

    let retry_after = resp4
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After header must be present")
        .to_str()
        .unwrap();
    assert!(retry_after.parse::<u64>().is_ok());

    assert_eq!(
        resp4.headers().get(X_RATELIMIT_REMAINING).unwrap().to_str().unwrap(),
        "0"
    );
    assert!(resp4.headers().contains_key(X_RATELIMIT_RESET));

    let body_bytes = axum::body::to_bytes(resp4.into_body(), 1024 * 64).await.unwrap();
    let err_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(err_json["error"]["code"], "RATE_LIMIT_EXCEEDED");

    // Request 5 from a DIFFERENT client IP: Allowed! (Bucket isolation)
    let req_diff_ip = Request::builder()
        .method("GET")
        .uri("/test-limited")
        .header("x-forwarded-for", "198.51.100.99")
        .body(Body::empty())
        .unwrap();

    let resp_diff = app.clone().oneshot(req_diff_ip).await.unwrap();
    assert_eq!(resp_diff.status(), StatusCode::OK);
    assert_eq!(
        resp_diff.headers().get(X_RATELIMIT_REMAINING).unwrap().to_str().unwrap(),
        "2"
    );
}
