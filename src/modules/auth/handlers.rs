use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sqlx::PgPool;
use validator::Validate;

use crate::config::AppConfig;
use crate::errors::AppError;
use crate::infra::redis::create_redis_pool;
use super::dto::{AuthUser, LoginRequest, RegisterRequest};
use super::middleware::{CurrentUser, SessionToken};
use super::oauth::{OAuthAuthorizeQuery, OAuthCallbackQuery, OAuthService};
use super::service::AuthService;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/register", post(register_handler))
        .route("/login", post(login_handler))
        .route("/me", get(me_handler))
        .route("/logout", post(logout_handler))
        .route("/oauth/:provider", get(oauth_authorize_handler))
        .route("/callback/:provider", get(oauth_callback_handler))
}

fn build_session_cookie(token: &str, max_age: i64) -> HeaderValue {
    let cookie_val = format!(
        "cabinet_session={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}",
        token, max_age
    );
    HeaderValue::from_str(&cookie_val).expect("Valid cookie header")
}

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

pub async fn me_handler(CurrentUser(user): CurrentUser) -> Result<Json<AuthUser>, AppError> {
    Ok(Json(user))
}

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

pub async fn oauth_authorize_handler(
    State(pool): State<PgPool>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthAuthorizeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let config = AppConfig::from_env().map_err(AppError::Internal)?;
    let redis = create_redis_pool(&config).await.ok();
    let mut oauth_service = OAuthService::new(pool, redis, config);

    let want_json = query.format.as_deref() == Some("json");
    let res = oauth_service.get_authorization_url(&provider, query).await?;

    if want_json {
        Ok((StatusCode::OK, HeaderMap::new(), Json(serde_json::to_value(res).unwrap())).into_response())
    } else {
        let mut headers = HeaderMap::new();
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
) -> Result<impl IntoResponse, AppError> {
    let config = AppConfig::from_env().map_err(AppError::Internal)?;
    let redis = create_redis_pool(&config).await.ok();
    let mut oauth_service = OAuthService::new(pool, redis, config);

    let (session, destination_url) = oauth_service.handle_callback(&provider, query).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        build_session_cookie(&session.token, 2592000),
    );
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&destination_url)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Invalid destination redirect: {}", e)))?,
    );

    Ok((StatusCode::FOUND, headers, ()))
}
