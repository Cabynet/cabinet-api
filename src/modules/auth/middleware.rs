use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use sqlx::PgPool;

use crate::errors::AppError;
use super::dto::AuthUser;
use super::service::AuthService;

pub struct CurrentUser(pub AuthUser);
pub struct OptionalCurrentUser(pub Option<AuthUser>);
pub struct SessionToken(pub String);

pub fn extract_token_from_parts(parts: &Parts) -> Option<String> {
    // 1. Check Authorization: Bearer <token>
    if let Some(auth_header) = parts.headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                let trimmed = token.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // 2. Check Cookie: cabinet_session=<token>
    if let Some(cookie_header) = parts.headers.get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for cookie in cookie_str.split(';') {
                let trimmed = cookie.trim();
                if let Some(val) = trimmed.strip_prefix("cabinet_session=") {
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }

    None
}

#[async_trait]
impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    PgPool: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pool = PgPool::from_ref(state);
        let token = extract_token_from_parts(parts)
            .ok_or_else(|| AppError::Unauthorized("Authentication required".to_string()))?;

        let auth_service = AuthService::new(pool);
        let user = auth_service.validate_session_token(&token).await?;

        Ok(CurrentUser(user))
    }
}

#[async_trait]
impl<S> FromRequestParts<S> for OptionalCurrentUser
where
    S: Send + Sync,
    PgPool: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pool = PgPool::from_ref(state);
        if let Some(token) = extract_token_from_parts(parts) {
            let auth_service = AuthService::new(pool);
            let user = auth_service.validate_session_token(&token).await.ok();
            Ok(OptionalCurrentUser(user))
        } else {
            Ok(OptionalCurrentUser(None))
        }
    }
}

#[async_trait]
impl<S> FromRequestParts<S> for SessionToken
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let token = extract_token_from_parts(parts)
            .ok_or_else(|| AppError::Unauthorized("Authentication token required".to_string()))?;
        Ok(SessionToken(token))
    }
}
