use base64::Engine;
use rand::RngCore;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::errors::AppError;
use super::dto::{AuthUser, SessionResponse};
use super::service::AuthService;

#[derive(Debug, Serialize, Deserialize)]
pub struct OAuthStateData {
    pub provider: String,
    pub code_verifier: String,
    pub client_type: String,
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct OAuthAuthorizeQuery {
    pub client_type: Option<String>,
    pub redirect_uri: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OAuthUrlResponse {
    pub provider: String,
    pub authorization_url: String,
    pub state: String,
}

pub struct OAuthService {
    pub pool: PgPool,
    pub redis: Option<redis::aio::ConnectionManager>,
    pub config: AppConfig,
}

impl OAuthService {
    pub fn new(
        pool: PgPool,
        redis: Option<redis::aio::ConnectionManager>,
        config: AppConfig,
    ) -> Self {
        Self { pool, redis, config }
    }

    pub fn generate_pkce() -> (String, String) {
        let mut verifier_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(code_verifier.as_bytes());
        let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

        (code_verifier, code_challenge)
    }

    pub fn generate_state() -> String {
        let mut state_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut state_bytes);
        hex::encode(state_bytes)
    }

    pub async fn get_authorization_url(
        &mut self,
        provider: &str,
        query: OAuthAuthorizeQuery,
    ) -> Result<OAuthUrlResponse, AppError> {
        let provider_lower = provider.to_lowercase();
        let (code_verifier, code_challenge) = Self::generate_pkce();
        let state = Self::generate_state();

        let client_type = query.client_type.unwrap_or_else(|| "WEB".to_string()).to_uppercase();

        let state_data = OAuthStateData {
            provider: provider_lower.clone(),
            code_verifier,
            client_type,
            redirect_uri: query.redirect_uri,
        };

        // Cache state in Redis with 10 min TTL
        if let Some(ref mut redis) = self.redis {
            let key = format!("oauth:state:{}", state);
            if let Ok(json_str) = serde_json::to_string(&state_data) {
                let _: Result<(), redis::RedisError> = redis.set_ex(key, json_str, 600).await;
            }
        }

        let callback_url = format!("{}/v1/auth/callback/{}", self.config.api_url.trim_end_matches('/'), provider_lower);

        let auth_url = match provider_lower.as_str() {
            "google" => {
                if self.config.google_client_id.is_empty() {
                    return Err(AppError::BadRequest("Google OAuth is not configured on this server".to_string()));
                }
                format!(
                    "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope=openid%20email%20profile&state={}&code_challenge={}&code_challenge_method=S256&prompt=select_account",
                    urlencoding::encode(&self.config.google_client_id),
                    urlencoding::encode(&callback_url),
                    urlencoding::encode(&state),
                    urlencoding::encode(&code_challenge),
                )
            }
            "github" => {
                if self.config.github_client_id.is_empty() {
                    return Err(AppError::BadRequest("GitHub OAuth is not configured on this server".to_string()));
                }
                format!(
                    "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=user:email%20read:user&state={}",
                    urlencoding::encode(&self.config.github_client_id),
                    urlencoding::encode(&callback_url),
                    urlencoding::encode(&state),
                )
            }
            _ => return Err(AppError::BadRequest(format!("Unsupported OAuth provider: {}", provider))),
        };

        Ok(OAuthUrlResponse {
            provider: provider_lower,
            authorization_url: auth_url,
            state,
        })
    }

    pub async fn handle_callback(
        &mut self,
        provider: &str,
        query: OAuthCallbackQuery,
    ) -> Result<(SessionResponse, String), AppError> {
        if let Some(err) = query.error {
            return Err(AppError::BadRequest(format!("OAuth provider error: {}", err)));
        }

        let code = query
            .code
            .ok_or_else(|| AppError::BadRequest("Missing authorization code".to_string()))?;
        let state = query
            .state
            .ok_or_else(|| AppError::BadRequest("Missing state parameter".to_string()))?;

        // 1. Retrieve state data from Redis
        let state_data: OAuthStateData = if let Some(ref mut redis) = self.redis {
            let key = format!("oauth:state:{}", state);
            let raw: Option<String> = redis.get(&key).await.ok();
            let _: Result<(), redis::RedisError> = redis.del(&key).await;
            raw.and_then(|s| serde_json::from_str(&s).ok())
                .ok_or_else(|| AppError::BadRequest("Invalid or expired OAuth state".to_string()))?
        } else {
            return Err(AppError::Internal(anyhow::anyhow!("Redis required for OAuth state tracking")));
        };

        let provider_lower = provider.to_lowercase();
        if state_data.provider != provider_lower {
            return Err(AppError::BadRequest("OAuth state provider mismatch".to_string()));
        }

        let callback_url = format!("{}/v1/auth/callback/{}", self.config.api_url.trim_end_matches('/'), provider_lower);

        // 2. Exchange code for user info
        let (email, name, avatar_url) = match provider_lower.as_str() {
            "google" => self.exchange_google(&code, &state_data.code_verifier, &callback_url).await?,
            "github" => self.exchange_github(&code).await?,
            _ => return Err(AppError::BadRequest("Unsupported provider".to_string())),
        };

        // 3. User Lookup or Auto-Creation
        let user = self.find_or_create_oauth_user(&email, &name, avatar_url).await?;

        // 4. Create Session
        let auth_service = AuthService::new(self.pool.clone());
        let session = auth_service
            .create_session(user, &state_data.client_type, None, None)
            .await?;

        // 5. Determine destination URL
        let destination_url = match state_data.client_type.as_str() {
            "MOBILE" | "DESKTOP" => {
                if let Some(custom_redirect) = state_data.redirect_uri {
                    format!("{}?token={}", custom_redirect, session.token)
                } else {
                    format!("cabinet://auth/callback?token={}", session.token)
                }
            }
            _ => {
                format!(
                    "{}/login/callback?token={}",
                    self.config.frontend_url.trim_end_matches('/'),
                    session.token
                )
            }
        };

        Ok((session, destination_url))
    }

    async fn exchange_google(
        &self,
        code: &str,
        code_verifier: &str,
        callback_url: &str,
    ) -> Result<(String, String, Option<String>), AppError> {
        let client = reqwest::Client::new();
        let token_resp = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", self.config.google_client_id.as_str()),
                ("client_secret", self.config.google_client_secret.as_str()),
                ("code", code),
                ("code_verifier", code_verifier),
                ("grant_type", "authorization_code"),
                ("redirect_uri", callback_url),
            ])
            .send()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to exchange Google token: {}", e)))?;

        let token_data: serde_json::Value = token_resp
            .json()
            .await
            .map_err(|e| AppError::BadRequest(format!("Invalid Google token response: {}", e)))?;

        let access_token = token_data["access_token"]
            .as_str()
            .ok_or_else(|| AppError::BadRequest("Google response missing access_token".to_string()))?;

        let userinfo_resp = client
            .get("https://www.googleapis.com/oauth2/v3/userinfo")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to fetch Google profile: {}", e)))?;

        let userinfo: serde_json::Value = userinfo_resp
            .json()
            .await
            .map_err(|e| AppError::BadRequest(format!("Invalid Google userinfo payload: {}", e)))?;

        let email = userinfo["email"]
            .as_str()
            .ok_or_else(|| AppError::BadRequest("Google profile missing email".to_string()))?
            .to_lowercase();

        let name = userinfo["name"]
            .as_str()
            .unwrap_or(email.split('@').next().unwrap_or("Google User"))
            .to_string();

        let avatar_url = userinfo["picture"].as_str().map(|s| s.to_string());

        Ok((email, name, avatar_url))
    }

    async fn exchange_github(
        &self,
        code: &str,
    ) -> Result<(String, String, Option<String>), AppError> {
        let client = reqwest::Client::new();
        let token_resp = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": self.config.github_client_id,
                "client_secret": self.config.github_client_secret,
                "code": code,
            }))
            .send()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to exchange GitHub token: {}", e)))?;

        let token_data: serde_json::Value = token_resp
            .json()
            .await
            .map_err(|e| AppError::BadRequest(format!("Invalid GitHub token response: {}", e)))?;

        let access_token = token_data["access_token"]
            .as_str()
            .ok_or_else(|| AppError::BadRequest("GitHub response missing access_token".to_string()))?;

        let user_resp = client
            .get("https://api.github.com/user")
            .header("User-Agent", "Cabinet-App")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to fetch GitHub profile: {}", e)))?;

        let user_data: serde_json::Value = user_resp
            .json()
            .await
            .map_err(|e| AppError::BadRequest(format!("Invalid GitHub profile payload: {}", e)))?;

        let name = user_data["name"]
            .as_str()
            .or_else(|| user_data["login"].as_str())
            .unwrap_or("GitHub User")
            .to_string();

        let avatar_url = user_data["avatar_url"].as_str().map(|s| s.to_string());

        // Extract primary verified email
        let email = if let Some(e) = user_data["email"].as_str() {
            e.to_lowercase()
        } else {
            let emails_resp = client
                .get("https://api.github.com/user/emails")
                .header("User-Agent", "Cabinet-App")
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to fetch GitHub emails: {}", e)))?;

            let emails: Vec<serde_json::Value> = emails_resp.json().await.unwrap_or_default();
            let primary = emails
                .iter()
                .find(|e| e["primary"].as_bool().unwrap_or(false) && e["verified"].as_bool().unwrap_or(false))
                .or_else(|| emails.first());

            primary
                .and_then(|p| p["email"].as_str())
                .ok_or_else(|| AppError::BadRequest("No email found on GitHub account".to_string()))?
                .to_lowercase()
        };

        Ok((email, name, avatar_url))
    }

    pub async fn find_or_create_oauth_user(
        &self,
        email: &str,
        name: &str,
        avatar_url: Option<String>,
    ) -> Result<AuthUser, AppError> {
        let existing = sqlx::query_as::<_, (Uuid, String, String, Option<String>)>(
            "SELECT id, email, name, avatar_url FROM users WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((id, email, current_name, current_avatar)) = existing {
            // Update verified status and avatar if not set
            let updated_avatar = current_avatar.or(avatar_url);
            sqlx::query(
                "UPDATE users SET email_verified = TRUE, avatar_url = COALESCE($2, avatar_url), updated_at = NOW() WHERE id = $1",
            )
            .bind(id)
            .bind(&updated_avatar)
            .execute(&self.pool)
            .await?;

            Ok(AuthUser {
                id,
                email,
                name: current_name,
                avatar_url: updated_avatar,
            })
        } else {
            let user_id = Uuid::now_v7();
            sqlx::query(
                r#"
                INSERT INTO users (id, email, email_verified, name, avatar_url, created_at, updated_at)
                VALUES ($1, $2, TRUE, $3, $4, NOW(), NOW())
                "#,
            )
            .bind(user_id)
            .bind(email)
            .bind(name)
            .bind(&avatar_url)
            .execute(&self.pool)
            .await?;

            Ok(AuthUser {
                id: user_id,
                email: email.to_string(),
                name: name.to_string(),
                avatar_url,
            })
        }
    }
}
