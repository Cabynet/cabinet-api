use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::dto::{AuthUser, LoginRequest, RegisterRequest, SessionResponse};
use crate::errors::AppError;

const SESSION_DURATION_DAYS: i64 = 30;

#[derive(Clone)]
pub struct AuthService {
    pub pool: PgPool,
}

impl AuthService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn hash_password(password: &str) -> Result<String, AppError> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Password hash error: {}", e)))
    }

    pub fn verify_password(password: &str, password_hash: &str) -> Result<bool, AppError> {
        let parsed_hash = PasswordHash::new(password_hash)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Invalid password hash: {}", e)))?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    }

    pub fn generate_token() -> (String, Vec<u8>) {
        let mut random_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut random_bytes);
        let raw_token = hex::encode(random_bytes);
        let token_hash = Self::hash_token(&raw_token);
        (raw_token, token_hash)
    }

    pub fn hash_token(raw_token: &str) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(raw_token.as_bytes());
        hasher.finalize().to_vec()
    }

    pub async fn register(
        &self,
        req: RegisterRequest,
        client_type: &str,
        user_agent: Option<String>,
        ip: Option<String>,
    ) -> Result<SessionResponse, AppError> {
        let email = req.email.trim().to_lowercase();
        let name = req.name.trim().to_string();

        let existing = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM users WHERE email = $1",
        )
        .bind(&email)
        .fetch_one(&self.pool)
        .await?;

        if existing > 0 {
            return Err(AppError::Conflict("Email already registered".to_string()));
        }

        let password_hash = Self::hash_password(&req.password)?;
        let user_id = Uuid::now_v7();

        sqlx::query(
            r#"
            INSERT INTO users (id, email, password_hash, name, created_at, updated_at)
            VALUES ($1, $2, $3, $4, NOW(), NOW())
            "#,
        )
        .bind(user_id)
        .bind(&email)
        .bind(&password_hash)
        .bind(&name)
        .execute(&self.pool)
        .await
        .map_err(|e| match e {
            // Lost the race with a concurrent registration of the same email.
            sqlx::Error::Database(ref db) if db.is_unique_violation() => {
                AppError::Conflict("Email already registered".to_string())
            }
            e => e.into(),
        })?;

        let user = AuthUser {
            id: user_id,
            email,
            name,
            avatar_url: None,
        };

        self.create_session(user, client_type, user_agent, ip).await
    }

    pub async fn login(
        &self,
        req: LoginRequest,
        client_type: &str,
        user_agent: Option<String>,
        ip: Option<String>,
    ) -> Result<SessionResponse, AppError> {
        let email = req.email.trim().to_lowercase();

        let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, Option<String>)>(
            r#"
            SELECT id, email, name, avatar_url, password_hash
            FROM users
            WHERE email = $1
            "#,
        )
        .bind(&email)
        .fetch_optional(&self.pool)
        .await?;

        let (user_id, email, name, avatar_url, password_hash) = match row {
            Some(r) => r,
            None => return Err(AppError::Unauthorized("Invalid email or password".to_string())),
        };

        let password_hash = password_hash
            .ok_or_else(|| AppError::Unauthorized("Account uses OAuth login".to_string()))?;

        if !Self::verify_password(&req.password, &password_hash)? {
            return Err(AppError::Unauthorized("Invalid email or password".to_string()));
        }

        let user = AuthUser {
            id: user_id,
            email,
            name,
            avatar_url,
        };

        self.create_session(user, client_type, user_agent, ip).await
    }

    pub async fn create_session(
        &self,
        user: AuthUser,
        client_type: &str,
        user_agent: Option<String>,
        ip: Option<String>,
    ) -> Result<SessionResponse, AppError> {
        let (raw_token, token_hash) = Self::generate_token();
        let session_id = Uuid::now_v7();
        let expires_at = Utc::now() + Duration::days(SESSION_DURATION_DAYS);

        sqlx::query(
            r#"
            INSERT INTO auth_sessions (
                id, user_id, token_hash, client_type, user_agent, ip_address, expires_at, created_at, last_active_at
            )
            VALUES ($1, $2, $3, $4, $5, $6::inet, $7, NOW(), NOW())
            "#,
        )
        .bind(session_id)
        .bind(user.id)
        .bind(&token_hash)
        .bind(client_type)
        .bind(user_agent)
        .bind(ip)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;

        Ok(SessionResponse {
            token: raw_token,
            user,
            expires_at,
        })
    }

    pub async fn validate_session_token(
        &self,
        raw_token: &str,
    ) -> Result<(AuthUser, super::dto::SessionMetadata), AppError> {
        let token_hash = Self::hash_token(raw_token);

        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                String,
                Option<String>,
                Uuid,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            r#"
            SELECT u.id, u.email, u.name, u.avatar_url, s.id, s.expires_at, s.created_at
            FROM auth_sessions s
            JOIN users u ON s.user_id = u.id
            WHERE s.token_hash = $1
              AND s.expires_at > NOW()
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((id, email, name, avatar_url, session_id, expires_at, created_at)) => Ok((
                AuthUser {
                    id,
                    email,
                    name,
                    avatar_url,
                },
                super::dto::SessionMetadata {
                    id: session_id,
                    user_id: id,
                    expires_at,
                    created_at,
                },
            )),
            None => Err(AppError::Unauthorized("Invalid or expired session".to_string())),
        }
    }

    pub async fn logout(&self, raw_token: &str) -> Result<(), AppError> {
        let token_hash = Self::hash_token(raw_token);
        sqlx::query(
            "DELETE FROM auth_sessions WHERE token_hash = $1",
        )
        .bind(&token_hash)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
