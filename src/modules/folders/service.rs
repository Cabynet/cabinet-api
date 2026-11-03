use chrono::Utc;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::dto::{CreateFolderRequest, FolderDto, UpdateFolderRequest};
use crate::errors::AppError;

#[derive(Clone)]
pub struct FoldersService {
    pub pool: PgPool,
}

impl FoldersService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_folders(&self, user_id: Uuid) -> Result<Vec<FolderDto>, AppError> {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            r#"
            SELECT to_jsonb(f)
                || jsonb_build_object(
                    'parent_id', CASE WHEN f.user_id = $1 THEN f.parent_id ELSE NULL END,
                    'access', CASE WHEN f.user_id = $1 THEN 'OWNER' ELSE m.role END,
                    'share_enabled', f.share_token_hash IS NOT NULL
                )
            FROM folders f
            LEFT JOIN folder_members m ON m.folder_id = f.id AND m.user_id = $1
            WHERE f.user_id = $1 OR m.user_id IS NOT NULL
            ORDER BY f.position, f.created_at
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|v| serde_json::from_value(crate::infra::camel::camelize(v)).map_err(|e| AppError::Internal(e.into())))
            .collect()
    }

    pub async fn create_folder(
        &self,
        user_id: Uuid,
        req: CreateFolderRequest,
    ) -> Result<FolderDto, AppError> {
        let folder_id = Uuid::now_v7();
        let name = req.name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::BadRequest("Folder name is required".into()));
        }

        if let Some(parent_id) = req.parent_id {
            let parent_exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM folders WHERE id = $1 AND user_id = $2",
            )
            .bind(parent_id)
            .bind(user_id)
            .fetch_one(&self.pool)
            .await?;

            if parent_exists == 0 {
                return Err(AppError::NotFound("Parent folder not found".to_string()));
            }
        }

        let max_pos: Option<i32> = sqlx::query_scalar(
            "SELECT MAX(position) FROM folders WHERE user_id = $1 AND parent_id IS NOT DISTINCT FROM $2",
        )
        .bind(user_id)
        .bind(req.parent_id)
        .fetch_one(&self.pool)
        .await?;
        let next_position = max_pos.unwrap_or(0) + 1;

        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<Uuid>,
                i32,
                chrono::DateTime<Utc>,
                chrono::DateTime<Utc>,
            ),
        >(
            r#"
            INSERT INTO folders (id, user_id, parent_id, name, description, position, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())
            RETURNING id, name, description, parent_id, position, created_at, updated_at
            "#,
        )
        .bind(folder_id)
        .bind(user_id)
        .bind(req.parent_id)
        .bind(&name)
        .bind(req.description)
        .bind(next_position)
        .fetch_one(&self.pool)
        .await?;

        Ok(FolderDto {
            id: row.0,
            name: row.1,
            description: row.2,
            parent_id: row.3,
            position: row.4,
            access: "OWNER".into(),
            share_enabled: false,
            created_at: row.5,
            updated_at: row.6,
        })
    }

    pub async fn update_folder(
        &self,
        user_id: Uuid,
        folder_id: Uuid,
        req: UpdateFolderRequest,
    ) -> Result<FolderDto, AppError> {
        let description = req.description.map(|d| d.filter(|d| !d.is_empty()));
        if description.as_ref().is_some_and(|d| d.as_ref().is_some_and(|d| d.chars().count() > 500)) {
            return Err(AppError::BadRequest("Description must not exceed 500 characters".into()));
        }
        if let Some(Some(parent)) = req.parent_id {
            let invalid: bool = sqlx::query_scalar(
                r#"
                WITH RECURSIVE descendants AS (
                    SELECT id FROM folders WHERE id = $1 AND user_id = $3
                    UNION ALL
                    SELECT f.id FROM folders f
                    JOIN descendants d ON f.parent_id = d.id
                    WHERE f.user_id = $3
                )
                SELECT EXISTS (SELECT 1 FROM descendants WHERE id = $2)
                    OR NOT EXISTS (SELECT 1 FROM folders WHERE id = $2 AND user_id = $3)
                "#,
            )
            .bind(folder_id)
            .bind(parent)
            .bind(user_id)
            .fetch_one(&self.pool)
            .await?;

            if invalid {
                return Err(AppError::BadRequest("Invalid parent folder".into()));
            }
        }

        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<Uuid>,
                i32,
                Option<Vec<u8>>,
                chrono::DateTime<Utc>,
                chrono::DateTime<Utc>,
            ),
        >(
            r#"
            UPDATE folders
            SET
                name = COALESCE($3, name),
                description = CASE WHEN $7 THEN $4 ELSE description END,
                parent_id = CASE WHEN $8 THEN $5 ELSE parent_id END,
                position = COALESCE($6, position),
                updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            RETURNING id, name, description, parent_id, position, share_token_hash, created_at, updated_at
            "#,
        )
        .bind(folder_id)
        .bind(user_id)
        .bind(req.name)
        .bind(description.clone().flatten())
        .bind(req.parent_id.flatten())
        .bind(req.position)
        .bind(description.is_some())
        .bind(req.parent_id.is_some())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Folder not found".to_string()))?;

        Ok(FolderDto {
            id: row.0,
            name: row.1,
            description: row.2,
            parent_id: row.3,
            position: row.4,
            access: "OWNER".into(),
            share_enabled: row.5.is_some(),
            created_at: row.6,
            updated_at: row.7,
        })
    }

    pub async fn delete_folder(&self, user_id: Uuid, folder_id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query("DELETE FROM folders WHERE id = $1 AND user_id = $2")
            .bind(folder_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(AppError::NotFound("Folder not found".to_string()));
        }

        Ok(())
    }

    pub async fn enable_share(&self, user_id: Uuid, folder_id: Uuid) -> Result<String, AppError> {
        let mut raw_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw_bytes);
        let raw_token = hex::encode(raw_bytes);

        let mut hasher = Sha256::new();
        hasher.update(raw_token.as_bytes());
        let token_hash = hasher.finalize().to_vec();

        let res = sqlx::query(
            r#"
            UPDATE folders
            SET share_token_hash = $3, share_token = $4, share_enabled_at = NOW(), updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(folder_id)
        .bind(user_id)
        .bind(&token_hash)
        .bind(&raw_token)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            return Err(AppError::NotFound("Folder not found".to_string()));
        }

        Ok(raw_token)
    }

    pub async fn revoke_share(&self, user_id: Uuid, folder_id: Uuid) -> Result<(), AppError> {
        let res = sqlx::query(
            r#"
            UPDATE folders
            SET share_token_hash = NULL, share_token = NULL, share_enabled_at = NULL, updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(folder_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            return Err(AppError::NotFound("Folder not found".to_string()));
        }

        Ok(())
    }

    pub async fn get_shared_folder(&self, raw_token: &str) -> Result<(FolderDto, Uuid), AppError> {
        let mut hasher = Sha256::new();
        hasher.update(raw_token.as_bytes());
        let token_hash = hasher.finalize().to_vec();

        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                Uuid,
                String,
                Option<String>,
                Option<Uuid>,
                i32,
                chrono::DateTime<Utc>,
                chrono::DateTime<Utc>,
            ),
        >(
            r#"
            SELECT id, user_id, name, description, parent_id, position, created_at, updated_at
            FROM folders
            WHERE share_token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::NotFound("Shared collection not found or link has expired".to_string())
        })?;

        Ok((
            FolderDto {
                id: row.0,
                name: row.2,
                description: row.3,
                parent_id: row.4,
                position: row.5,
                access: "VIEWER".into(),
                share_enabled: true,
                created_at: row.6,
                updated_at: row.7,
            },
            row.1, // user_id
        ))
    }
}
