use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sha2::Digest;
use sqlx::PgPool;
use validator::Validate;

use crate::config::AppConfig;
use crate::errors::AppError;
use super::dto::{LoginRequest, MeResponse, RegisterRequest};
use super::middleware::{CurrentSession, CurrentUser, SessionToken};
use super::oauth::{OAuthAuthorizeQuery, OAuthCallbackQuery, OAuthService};
use super::service::AuthService;

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/register", post(register_handler))
        .route("/login", post(login_handler))
        .route("/me", get(me_handler))
        .route("/logout", post(logout_handler))
        .route("/sign-in/social", post(sign_in_social_handler))
        .route("/accounts", get(list_accounts))
        .route("/oauth/:provider", get(oauth_authorize_handler))
        .route("/callback/:provider", get(oauth_callback_handler))
}

#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignInSocialRequest {
    pub provider: String,
    #[serde(alias = "callback_url")]
    pub callback_url: Option<String>,
    #[serde(rename = "callbackURL")]
    pub callback_url_camel: Option<String>,
}

#[utoipa::path(
    post,
    path = "/v1/auth/sign-in/social",
    request_body = SignInSocialRequest,
    responses(
        (status = 200, description = "OAuth authorization URL", body = serde_json::Value)
    ),
    tag = "auth"
)]
pub async fn sign_in_social_handler(
    State(pool): State<PgPool>,
    Json(payload): Json<SignInSocialRequest>,
) -> Result<impl IntoResponse, AppError> {
    let config = AppConfig::shared().map_err(AppError::Internal)?.clone();
    let mut oauth_service = OAuthService::new(pool, crate::infra::redis::shared().await, config);

    let redirect_uri = payload.callback_url_camel.or(payload.callback_url);

    let query = OAuthAuthorizeQuery {
        client_type: Some("WEB".to_string()),
        redirect_uri,
        format: Some("json".to_string()),
    };

    let res = oauth_service.get_authorization_url(&payload.provider, query).await?;

    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, oauth_nonce_cookie(&res.nonce));
    Ok((headers, Json(serde_json::json!({
        "url": res.authorization_url,
        "redirect": false
    }))))
}

const OAUTH_NONCE_COOKIE: &str = "cabinet_oauth_nonce";

/// Short-lived cookie tying an OAuth callback to the browser that started it; an empty nonce clears it.
fn oauth_nonce_cookie(nonce: &str) -> HeaderValue {
    let max_age = if nonce.is_empty() { 0 } else { super::OAUTH_STATE_TTL_SECS };
    let mut cookie = format!("{OAUTH_NONCE_COOKIE}={nonce}; HttpOnly; Path=/v1/auth; SameSite=Lax; Max-Age={max_age}");
    if std::env::var("API_URL").is_ok_and(|u| u.starts_with("https://")) {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).expect("Valid cookie header")
}

pub(crate) fn build_session_cookie(token: &str, max_age: i64) -> HeaderValue {
    let mut cookie_val = format!(
        "cabinet_session={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}",
        token, max_age
    );
    if let Ok(config) = AppConfig::shared() {
        if config.api_url.starts_with("https://") {
            cookie_val.push_str("; Secure");
        }
        if let Some(domain) = &config.cookie_domain {
            cookie_val.push_str(&format!("; Domain={}", domain.trim()));
        }
    }
    HeaderValue::from_str(&cookie_val).expect("Valid cookie header")
}

#[utoipa::path(
    post,
    path = "/v1/auth/register",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "Registration successful", body = SessionResponse),
        (status = 400, description = "Validation error", body = ErrorResponse),
        (status = 409, description = "Email already registered", body = ErrorResponse)
    ),
    tag = "auth"
)]
pub async fn register_handler(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(payload): Json<RegisterRequest>,
) -> Result<impl IntoResponse, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let auth_service = AuthService::new(pool);
    let session = auth_service
        .register(payload, "WEB", user_agent, None)
        .await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        build_session_cookie(&session.token, 2592000), // 30 days
    );

    Ok((StatusCode::CREATED, response_headers, Json(session)))
}

#[utoipa::path(
    post,
    path = "/v1/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Login successful", body = SessionResponse),
        (status = 401, description = "Invalid credentials", body = ErrorResponse)
    ),
    tag = "auth"
)]
pub async fn login_handler(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Per-email bucket on top of the per-IP one, so a botnet can't brute-force a single account.
    if let Some(mut redis) = crate::infra::redis::shared().await {
        let digest = hex::encode(&sha2::Sha256::digest(payload.email.trim().to_lowercase().as_bytes())[..8]);
        let key = format!("cabinet:ratelimit:login-email:{digest}");
        if let Ok(status) = crate::infra::rate_limit::RedisRateLimiter::check_limit(&mut redis, &key, 10, 900).await {
            if !status.allowed {
                return Err(AppError::TooManyRequests("Too many login attempts for this account. Try again later.".into()));
            }
        }
    }

    let auth_service = AuthService::new(pool);
    let session = auth_service
        .login(payload, "WEB", user_agent, None)
        .await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        build_session_cookie(&session.token, 2592000),
    );

    Ok((StatusCode::OK, response_headers, Json(session)))
}

#[utoipa::path(
    get,
    path = "/v1/auth/me",
    responses(
        (status = 200, description = "Current authenticated user and active session metadata", body = MeResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(
        ("bearer_auth" = []),
        ("cookie_auth" = [])
    ),
    tag = "auth"
)]
pub async fn me_handler(
    CurrentUser(user): CurrentUser,
    CurrentSession(session): CurrentSession,
) -> Result<Json<MeResponse>, AppError> {
    Ok(Json(MeResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        avatar_url: user.avatar_url,
        session,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/auth/logout",
    responses(
        (status = 200, description = "Logged out successfully")
    ),
    tag = "auth"
)]
pub async fn logout_handler(
    State(pool): State<PgPool>,
    SessionToken(token): SessionToken,
) -> Result<impl IntoResponse, AppError> {
    let auth_service = AuthService::new(pool);
    auth_service.logout(&token).await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::SET_COOKIE, build_session_cookie("", 0));

    Ok((
        StatusCode::OK,
        response_headers,
        Json(serde_json::json!({ "success": true })),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/auth/oauth/{provider}",
    params(
        ("provider" = String, Path, description = "OAuth provider: google | github"),
        OAuthAuthorizeQuery
    ),
    responses(
        (status = 200, description = "OAuth authorization URL when format=json", body = OAuthUrlResponse),
        (status = 302, description = "Redirect to provider OAuth login")
    ),
    tag = "auth"
)]
pub async fn oauth_authorize_handler(
    State(pool): State<PgPool>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthAuthorizeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let config = AppConfig::shared().map_err(AppError::Internal)?.clone();
    let mut oauth_service = OAuthService::new(pool, crate::infra::redis::shared().await, config);

    let want_json = query.format.as_deref() == Some("json");
    let res = oauth_service.get_authorization_url(&provider, query).await?;

    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, oauth_nonce_cookie(&res.nonce));
    if want_json {
        Ok((StatusCode::OK, headers, Json(serde_json::to_value(res).unwrap())).into_response())
    } else {
        headers.insert(
            header::LOCATION,
            HeaderValue::from_str(&res.authorization_url)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Invalid redirect URL: {}", e)))?,
        );
        Ok((StatusCode::FOUND, headers, ()).into_response())
    }
}

pub async fn oauth_callback_handler(
    State(pool): State<PgPool>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthCallbackQuery>,
    request_headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let config = AppConfig::shared().map_err(AppError::Internal)?.clone();
    let mut oauth_service = OAuthService::new(pool, crate::infra::redis::shared().await, config);

    let nonce = request_headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(concat!("cabinet_oauth_nonce", "=")));
    let (session, destination_url) = oauth_service.handle_callback(&provider, query, nonce).await?;

    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, build_session_cookie(&session.token, 2592000));
    headers.append(header::SET_COOKIE, oauth_nonce_cookie(""));
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&destination_url)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Invalid destination redirect: {}", e)))?,
    );

    Ok((StatusCode::FOUND, headers, ()))
}

async fn list_accounts(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        r#"
        SELECT jsonb_build_object(
            'id', provider,
            'accountId', provider,
            'providerId', provider,
            'createdAt', created_at
        )
        FROM oauth_accounts
        WHERE user_id = $1
        ORDER BY created_at
        "#,
    )
    .bind(user.id)
    .fetch_all(&pool)
    .await?;

    Ok(Json(rows))
}
