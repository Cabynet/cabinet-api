use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::auth::dto::AuthUser;
use crate::modules::auth::service::AuthService;
use super::dto::{ChangePasswordRequest, UpdateUserRequest, UserActionResponse};

#[derive(Clone)]
pub struct UsersService {
    pub pool: PgPool,
}

impl UsersService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn update_profile(
        &self,
        user_id: Uuid,
        req: UpdateUserRequest,
    ) -> Result<AuthUser, AppError> {
        let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>)>(
            r#"
            UPDATE users
            SET
                name = COALESCE($2, name),
                avatar_url = COALESCE($3, avatar_url),
                updated_at = NOW()
            WHERE id = $1
            RETURNING id, email, name, avatar_url
            "#,
        )
        .bind(user_id)
        .bind(req.name.as_deref().map(|s| s.trim()))
        .bind(req.avatar_url.as_deref().map(|s| s.trim()))
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

        Ok(AuthUser {
            id: row.0,
            email: row.1,
            name: row.2,
            avatar_url: row.3,
        })
    }

    pub async fn change_password(
        &self,
        user_id: Uuid,
        req: ChangePasswordRequest,
    ) -> Result<UserActionResponse, AppError> {
        let current_hash: Option<String> = sqlx::query_scalar(
            "SELECT password_hash FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

        if let Some(hash) = current_hash {
            let current_pw = req.current_password.unwrap_or_default();
            if !AuthService::verify_password(&current_pw, &hash)? {
                return Err(AppError::Unauthorized("Current password incorrect".to_string()));
            }
        }

        let new_hash = AuthService::hash_password(&req.new_password)?;

        sqlx::query(
            "UPDATE users SET password_hash = $2, updated_at = NOW() WHERE id = $1",
        )
        .bind(user_id)
        .bind(&new_hash)
        .execute(&self.pool)
        .await?;

        Ok(UserActionResponse {
            success: true,
            message: "Password updated successfully".to_string(),
        })
    }

    pub async fn delete_account(&self, user_id: Uuid) -> Result<UserActionResponse, AppError> {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        Ok(UserActionResponse {
            success: true,
            message: "Account deleted successfully".to_string(),
        })
    }
}
