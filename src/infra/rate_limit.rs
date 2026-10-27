use axum::{
    extract::Request,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const X_RATELIMIT_LIMIT: &str = "x-ratelimit-limit";
pub const X_RATELIMIT_REMAINING: &str = "x-ratelimit-remaining";
pub const X_RATELIMIT_RESET: &str = "x-ratelimit-reset";

#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    pub prefix: &'static str,
    pub max_requests: i64,
    pub window_seconds: i64,
}

impl RateLimitConfig {
    pub const AUTH: Self = Self {
        prefix: "auth",
        max_requests: 10,
        window_seconds: 60,
    };

    pub const SAVES: Self = Self {
        prefix: "saves",
        max_requests: 60,
        window_seconds: 60,
    };

    pub const SEARCH: Self = Self {
        prefix: "search",
        max_requests: 30,
        window_seconds: 60,
    };

    pub const GENERAL: Self = Self {
        prefix: "general",
        max_requests: 120,
        window_seconds: 60,
    };
}

#[derive(Debug, Clone)]
pub struct RateLimitStatus {
    pub allowed: bool,
    pub limit: i64,
    pub remaining: i64,
    pub reset_seconds: i64,
}

pub struct RedisRateLimiter;

impl RedisRateLimiter {
    pub async fn check_limit(
        redis: &mut ConnectionManager,
        key: &str,
        max_requests: i64,
        window_seconds: i64,
    ) -> Result<RateLimitStatus, redis::RedisError> {
        let count: i64 = redis.incr(key, 1).await?;

        if count == 1 {
            let _: Result<(), redis::RedisError> = redis.expire(key, window_seconds).await;
        }

        let ttl: i64 = redis.ttl(key).await.unwrap_or(window_seconds);
        let reset_seconds = if ttl > 0 { ttl } else { window_seconds };

        let allowed = count <= max_requests;
        let remaining = (max_requests - count).max(0);

        Ok(RateLimitStatus {
            allowed,
            limit: max_requests,
            remaining,
            reset_seconds,
        })
    }
}

pub fn extract_client_identifier(req: &Request) -> String {
    // 1. If Authorization header present, hash token as identifier
    if let Some(auth_val) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(s) = auth_val.to_str() {
            if let Some(tok) = s.strip_prefix("Bearer ") {
                let mut hasher = Sha256::new();
                hasher.update(tok.trim().as_bytes());
                return format!("usr:{}", hex::encode(&hasher.finalize()[..8]));
            }
        }
    }

    // 2. If cookie present, hash cookie token as identifier
    if let Some(cookie_val) = req.headers().get(header::COOKIE) {
        if let Ok(s) = cookie_val.to_str() {
            for part in s.split(';') {
                if let Some(tok) = part.trim().strip_prefix("cabinet_session=") {
                    let mut hasher = Sha256::new();
                    hasher.update(tok.trim().as_bytes());
                    return format!("usr:{}", hex::encode(&hasher.finalize()[..8]));
                }
            }
        }
    }

    // 3. Fallback to Client IP headers
    for header_name in ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"] {
        if let Some(val) = req.headers().get(header_name) {
            if let Ok(s) = val.to_str() {
                let ip = s.split(',').next().unwrap_or("").trim();
                if !ip.is_empty() {
                    return format!("ip:{}", ip);
                }
            }
        }
    }

    "ip:unknown".to_string()
}

pub async fn rate_limit_middleware(
    mut redis: ConnectionManager,
    config: RateLimitConfig,
    req: Request,
    next: Next,
) -> Response {
    let client_id = extract_client_identifier(&req);
    let key = format!("cabinet:ratelimit:{}:{}", config.prefix, client_id);

    match RedisRateLimiter::check_limit(&mut redis, &key, config.max_requests, config.window_seconds).await {
        Ok(status) => {
            if !status.allowed {
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::RETRY_AFTER,
                    HeaderValue::from_str(&status.reset_seconds.to_string()).unwrap(),
                );
                headers.insert(
                    HeaderName::from_static(X_RATELIMIT_LIMIT),
                    HeaderValue::from_str(&status.limit.to_string()).unwrap(),
                );
                headers.insert(
                    HeaderName::from_static(X_RATELIMIT_REMAINING),
                    HeaderValue::from_static("0"),
                );
                headers.insert(
                    HeaderName::from_static(X_RATELIMIT_RESET),
                    HeaderValue::from_str(&status.reset_seconds.to_string()).unwrap(),
                );

                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    Json(json!({
                        "error": {
                            "code": "RATE_LIMIT_EXCEEDED",
                            "message": "Too many requests. Please slow down and try again later.",
                            "retry_after_seconds": status.reset_seconds
                        }
                    })),
                )
                    .into_response();
            }

            let mut resp = next.run(req).await;
            let resp_headers = resp.headers_mut();
            if let Ok(val) = HeaderValue::from_str(&status.limit.to_string()) {
                resp_headers.insert(HeaderName::from_static(X_RATELIMIT_LIMIT), val);
            }
            if let Ok(val) = HeaderValue::from_str(&status.remaining.to_string()) {
                resp_headers.insert(HeaderName::from_static(X_RATELIMIT_REMAINING), val);
            }
            if let Ok(val) = HeaderValue::from_str(&status.reset_seconds.to_string()) {
                resp_headers.insert(HeaderName::from_static(X_RATELIMIT_RESET), val);
            }

            resp
        }
        Err(e) => {
            tracing::warn!(error = ?e, "Redis rate limit check failed; failing open");
            next.run(req).await
        }
    }
}
