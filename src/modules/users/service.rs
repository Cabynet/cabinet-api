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
        current_session_id: Uuid,
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

        // A stolen session must not survive a password change.
        sqlx::query("DELETE FROM auth_sessions WHERE user_id = $1 AND id <> $2")
            .bind(user_id)
            .bind(current_session_id)
            .execute(&self.pool)
            .await?;

        Ok(UserActionResponse {
            success: true,
            message: "Password updated successfully".to_string(),
        })
    }

    pub async fn delete_account(
        &self,
        user_id: Uuid,
        current_password: Option<String>,
    ) -> Result<UserActionResponse, AppError> {
        // Re-authenticate: a stolen session alone must not be able to destroy the account.
        let hash: Option<String> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        if let Some(hash) = hash {
            if !AuthService::verify_password(&current_password.unwrap_or_default(), &hash)? {
                return Err(AppError::Unauthorized("Current password incorrect".to_string()));
            }
        }

        // Objects another user's copied bookmark still points at must survive the account.
        let keys: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT m.object_key FROM media_assets m
            WHERE m.user_id = $1
              AND NOT EXISTS (
                SELECT 1 FROM bookmarks o
                WHERE o.user_id <> $1 AND right(o.raw_url, length(m.object_key)) = m.object_key)
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        crate::modules::media::service::delete_objects(keys).await;

        Ok(UserActionResponse {
            success: true,
            message: "Account deleted successfully".to_string(),
        })
    }
}
