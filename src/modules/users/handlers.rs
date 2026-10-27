use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{patch, post},
    Json, Router,
};
use sqlx::PgPool;
use validator::Validate;

use crate::errors::{AppError, ErrorResponse};
use crate::modules::auth::dto::AuthUser;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{ChangePasswordRequest, UpdateUserRequest, UserActionResponse};
use super::service::UsersService;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/me", patch(update_profile_handler).delete(delete_account_handler))
        .route("/change-password", post(change_password_handler))
}

#[utoipa::path(
    patch,
    path = "/v1/users/me",
    request_body = UpdateUserRequest,
    responses(
        (status = 200, description = "Profile updated successfully", body = AuthUser)
    ),
    tag = "users"
)]
pub async fn update_profile_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<Json<AuthUser>, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = UsersService::new(pool);
    let updated = service.update_profile(user.id, payload).await?;
    Ok(Json(updated))
}

#[utoipa::path(
    post,
    path = "/v1/users/change-password",
    request_body = ChangePasswordRequest,
    responses(
        (status = 200, description = "Password updated successfully", body = UserActionResponse),
        (status = 401, description = "Current password incorrect", body = ErrorResponse)
    ),
    tag = "users"
)]
pub async fn change_password_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<ChangePasswordRequest>,
) -> Result<Json<UserActionResponse>, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = UsersService::new(pool);
    let res = service.change_password(user.id, payload).await?;
    Ok(Json(res))
}

#[utoipa::path(
    delete,
    path = "/v1/users/me",
    responses(
        (status = 200, description = "Account deleted successfully", body = UserActionResponse)
    ),
    tag = "users"
)]
pub async fn delete_account_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, AppError> {
    let service = UsersService::new(pool);
    let res = service.delete_account(user.id).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_static("cabinet_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0"),
    );

    Ok((StatusCode::OK, headers, Json(res)))
}
