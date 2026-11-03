use axum::{
    extract::Request,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use redis::aio::ConnectionManager;
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
        max_requests: 15,
        window_seconds: 60,
    };

    pub const MEDIA: Self = Self {
        prefix: "media",
        max_requests: 300,
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
        max_requests: 180,
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
        // Keep increment and expiry atomic, and repair legacy counters without a TTL.
        let (count, ttl): (i64, i64) = redis::Script::new(
            r#"
            local count = redis.call('INCR', KEYS[1])
            local ttl = redis.call('TTL', KEYS[1])
            if ttl < 0 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
                ttl = tonumber(ARGV[1])
            end
            return {count, ttl}
            "#,
        )
        .key(key)
        .arg(window_seconds)
        .invoke_async(redis)
        .await?;
        let reset_seconds = ttl.max(1);

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

pub fn extract_client_identifier(req: &Request, by_ip_only: bool) -> String {
    // Tokens are unvalidated here, so a caller can mint a fresh bucket per request; auth limits must use IP only.
    // 1. If Authorization header present, hash token as identifier
    if let Some(auth_val) = req.headers().get(header::AUTHORIZATION).filter(|_| !by_ip_only) {
        if let Ok(s) = auth_val.to_str() {
            if let Some(tok) = s.strip_prefix("Bearer ") {
                let mut hasher = Sha256::new();
                hasher.update(tok.trim().as_bytes());
                return format!("usr:{}", hex::encode(&hasher.finalize()[..8]));
            }
        }
    }

    // 2. If cookie present, hash cookie token as identifier
    if let Some(cookie_val) = req.headers().get(header::COOKIE).filter(|_| !by_ip_only) {
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

    // 3. Forwarded-for headers are client-controlled unless a trusted reverse proxy sets them.
    let trusted_proxy = std::env::var("TRUSTED_PROXY").is_ok_and(|v| v == "true");
    for header_name in ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"].into_iter().filter(|_| trusted_proxy) {
        if let Some(val) = req.headers().get(header_name) {
            if let Ok(s) = val.to_str() {
                let ip = s.split(',').next().unwrap_or("").trim();
                if !ip.is_empty() {
                    return format!("ip:{}", ip);
                }
            }
        }
    }

    // 4. Fallback to TCP ConnectInfo<SocketAddr>
    if let Some(connect_info) = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>() {
        return format!("ip:{}", connect_info.0.ip());
    }

    "ip:unknown".to_string()
}

pub async fn rate_limit_middleware(
    mut redis: ConnectionManager,
    config: RateLimitConfig,
    req: Request,
    next: Next,
) -> Response {
    let effective_config = if config.prefix == RateLimitConfig::GENERAL.prefix {
        let path = req.uri().path();
        if path.ends_with("/proxy-image") {
            RateLimitConfig::MEDIA
        } else if path == "/v1/auth/login" || path == "/v1/auth/register" {
            RateLimitConfig::AUTH
        } else {
            config
        }
    } else {
        config
    };

    let client_id = extract_client_identifier(&req, effective_config.prefix == RateLimitConfig::AUTH.prefix);
    let key = format!("cabinet:ratelimit:{}:{}", effective_config.prefix, client_id);

    match RedisRateLimiter::check_limit(&mut redis, &key, effective_config.max_requests, effective_config.window_seconds).await {
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
                            "retryAfterSeconds": status.reset_seconds
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
