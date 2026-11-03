use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
mod common;
use cabinet_api::{
    infra::{
        rate_limit::{
            rate_limit_middleware, RateLimitConfig, RedisRateLimiter, X_RATELIMIT_LIMIT, X_RATELIMIT_REMAINING,
            X_RATELIMIT_RESET,
        },
        redis::create_redis_pool,
    },
};
use redis::AsyncCommands;
use tower::ServiceExt;

#[tokio::test]
async fn rate_limiter_repairs_counters_without_expiry_and_allows_requests_after_reset() {
    let config = common::config();
    let mut redis = create_redis_pool(&config).await.expect("Isolated test Redis is required");
    let key = format!("cabinet:ratelimit:test:orphan:{}", uuid::Uuid::new_v4());
    let _: () = redis.set(&key, 164).await.unwrap();
    let ttl: i64 = redis.ttl(&key).await.unwrap();
    assert_eq!(ttl, -1);

    let blocked = RedisRateLimiter::check_limit(&mut redis, &key, 120, 1).await.unwrap();
    assert!(!blocked.allowed);
    assert_eq!(blocked.remaining, 0);
    assert_eq!(blocked.reset_seconds, 1);
    let ttl: i64 = redis.ttl(&key).await.unwrap();
    assert!((0..=1).contains(&ttl));

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let reset = RedisRateLimiter::check_limit(&mut redis, &key, 120, 60).await.unwrap();
    assert!(reset.allowed);
    assert_eq!(reset.remaining, 119);
    let ttl: i64 = redis.ttl(&key).await.unwrap();
    assert!(ttl > 0 && ttl <= 60);
    let _: () = redis.del(&key).await.unwrap();
}

#[tokio::test]
async fn test_redis_token_bucket_rate_limiter_returns_429() {
    // Forwarded-for headers are only honoured behind a trusted proxy.
    std::env::set_var("TRUSTED_PROXY", "true");
    let config = common::config();
    let redis = create_redis_pool(&config).await.expect("Isolated test Redis is required");

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

#[tokio::test]
async fn test_rate_limiter_extracts_tcp_peer_ip_and_specializes_routes() {
    let config = common::config();
    let redis = create_redis_pool(&config).await.expect("Isolated test Redis is required");

    let redis_for_mw = redis.clone();
    let app = Router::new()
        .route("/v1/auth/login", get(|| async { StatusCode::OK }))
        .route("/v1/bookmarks/proxy-image", get(|| async { StatusCode::OK }))
        .route("/v1/bookmarks", get(|| async { StatusCode::OK }))
        .layer(middleware::from_fn(move |req, next| {
            rate_limit_middleware(redis_for_mw.clone(), RateLimitConfig::GENERAL, req, next)
        }));

    // 1. TCP ConnectInfo without proxy headers resolves to socket IP and uses GENERAL limit (180)
    let peer_addr: std::net::SocketAddr = "192.0.2.45:54321".parse().unwrap();
    let mut req_tcp = Request::builder()
        .method("GET")
        .uri("/v1/bookmarks")
        .body(Body::empty())
        .unwrap();
    req_tcp.extensions_mut().insert(axum::extract::ConnectInfo(peer_addr));

    let resp_tcp = app.clone().oneshot(req_tcp).await.unwrap();
    assert_eq!(resp_tcp.status(), StatusCode::OK);
    assert_eq!(
        resp_tcp.headers().get(X_RATELIMIT_LIMIT).unwrap().to_str().unwrap(),
        "180"
    );

    // 2. /v1/auth/login route specializes to RateLimitConfig::AUTH (15)
    let mut req_auth = Request::builder()
        .method("GET")
        .uri("/v1/auth/login")
        .body(Body::empty())
        .unwrap();
    req_auth.extensions_mut().insert(axum::extract::ConnectInfo(peer_addr));

    let resp_auth = app.clone().oneshot(req_auth).await.unwrap();
    assert_eq!(resp_auth.status(), StatusCode::OK);
    assert_eq!(
        resp_auth.headers().get(X_RATELIMIT_LIMIT).unwrap().to_str().unwrap(),
        "15"
    );

    // 3. /v1/bookmarks/proxy-image route specializes to RateLimitConfig::MEDIA (300)
    let mut req_media = Request::builder()
        .method("GET")
        .uri("/v1/bookmarks/proxy-image")
        .body(Body::empty())
        .unwrap();
    req_media.extensions_mut().insert(axum::extract::ConnectInfo(peer_addr));

    let resp_media = app.clone().oneshot(req_media).await.unwrap();
    assert_eq!(resp_media.status(), StatusCode::OK);
    assert_eq!(
        resp_media.headers().get(X_RATELIMIT_LIMIT).unwrap().to_str().unwrap(),
        "300"
    );
}
