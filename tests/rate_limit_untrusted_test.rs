use axum::{body::Body, http::Request, middleware, routing::get, Router};
mod common;
use cabinet_api::infra::{
    rate_limit::{rate_limit_middleware, RateLimitConfig, X_RATELIMIT_REMAINING},
    redis::create_redis_pool,
};
use tower::ServiceExt;

// This binary never sets TRUSTED_PROXY, so spoofed forwarded-for headers must not mint new buckets.
#[tokio::test]
async fn forwarded_headers_and_bearer_tokens_cannot_mint_auth_buckets() {
    let config = common::config();
    let redis = create_redis_pool(&config).await.expect("Isolated test Redis is required");
    let cfg = RateLimitConfig { prefix: "auth", max_requests: 2, window_seconds: 30 };
    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(middleware::from_fn(move |req, next| rate_limit_middleware(redis.clone(), cfg, req, next)));
    let peer: std::net::SocketAddr = "192.0.2.77:1111".parse().unwrap();
    let mut statuses = vec![];
    for i in 0..4 {
        let mut req = Request::builder()
            .uri("/x")
            .header("x-forwarded-for", format!("203.0.113.{i}"))
            .header("authorization", format!("Bearer random-{i}"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
        let resp = app.clone().oneshot(req).await.unwrap();
        statuses.push((resp.status().as_u16(), resp.headers().get(X_RATELIMIT_REMAINING).map(|v| v.to_str().unwrap().to_string())));
    }
    assert_eq!(statuses[2].0, 429, "third request from the same peer must be limited: {statuses:?}");
}
