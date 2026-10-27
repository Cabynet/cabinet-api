use chrono::Utc;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use super::dto::{CreateFolderRequest, FolderDto, UpdateFolderRequest};

#[derive(Clone)]
pub struct FoldersService {
    pub pool: PgPool,
}

impl FoldersService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_folders(&self, user_id: Uuid) -> Result<Vec<FolderDto>, AppError> {
        let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<Uuid>, i32, Option<Vec<u8>>, chrono::DateTime<Utc>, chrono::DateTime<Utc>)>(
            r#"
            SELECT id, name, description, parent_id, position, share_token_hash, created_at, updated_at
            FROM folders
            WHERE user_id = $1
            ORDER BY position ASC, created_at ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let folders = rows
            .into_iter()
            .map(|(id, name, description, parent_id, position, share_token_hash, created_at, updated_at)| {
                FolderDto {
                    id,
                    name,
                    description,
                    parent_id,
                    position,
                    share_enabled: share_token_hash.is_some(),
                    created_at,
                    updated_at,
                }
            })
            .collect();

        Ok(folders)
    }

    pub async fn create_folder(
        &self,
        user_id: Uuid,
        req: CreateFolderRequest,
    ) -> Result<FolderDto, AppError> {
        let req = req.resolved();
        let folder_id = Uuid::now_v7();
        let name = req.name.unwrap_or_else(|| "Untitled Folder".to_string()).trim().to_string();

        if let Some(parent_id) = req.parent_id {
            let parent_exists = sqlx::query_scalar::<_, i64>(
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

        let max_pos = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(position) FROM folders WHERE user_id = $1 AND parent_id IS NOT DISTINCT FROM $2",
        )
        .bind(user_id)
        .bind(req.parent_id)
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<Uuid>, i32, chrono::DateTime<Utc>, chrono::DateTime<Utc>)>(
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
        .bind(max_pos + 1)
        .fetch_one(&self.pool)
        .await?;

        Ok(FolderDto {
            id: row.0,
            name: row.1,
            description: row.2,
            parent_id: row.3,
            position: row.4,
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
        let req = req.resolved();
        let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<Uuid>, i32, Option<Vec<u8>>, chrono::DateTime<Utc>, chrono::DateTime<Utc>)>(
            r#"
            UPDATE folders
            SET
                name = COALESCE($3, name),
                description = COALESCE($4, description),
                parent_id = COALESCE($5, parent_id),
                position = COALESCE($6, position),
                updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            RETURNING id, name, description, parent_id, position, share_token_hash, created_at, updated_at
            "#,
        )
        .bind(folder_id)
        .bind(user_id)
        .bind(req.name)
        .bind(req.description)
        .bind(req.parent_id)
        .bind(req.position)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Folder not found".to_string()))?;

        Ok(FolderDto {
            id: row.0,
            name: row.1,
            description: row.2,
            parent_id: row.3,
            position: row.4,
            share_enabled: row.5.is_some(),
            created_at: row.6,
            updated_at: row.7,
        })
    }

    pub async fn delete_folder(&self, user_id: Uuid, folder_id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "DELETE FROM folders WHERE id = $1 AND user_id = $2",
        )
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
        let mut raw_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut raw_bytes);
        let raw_token = hex::encode(raw_bytes);

        let mut hasher = Sha256::new();
        hasher.update(raw_token.as_bytes());
        let token_hash = hasher.finalize().to_vec();

        let res = sqlx::query(
            r#"
            UPDATE folders
            SET share_token_hash = $3, share_enabled_at = NOW(), updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(folder_id)
        .bind(user_id)
        .bind(&token_hash)
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
            SET share_token_hash = NULL, share_enabled_at = NULL, updated_at = NOW()
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

        let row = sqlx::query_as::<_, (Uuid, Uuid, String, Option<String>, Option<Uuid>, i32, chrono::DateTime<Utc>, chrono::DateTime<Utc>)>(
            r#"
            SELECT id, user_id, name, description, parent_id, position, created_at, updated_at
            FROM folders
            WHERE share_token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Shared collection not found or link has expired".to_string()))?;

        Ok((
            FolderDto {
                id: row.0,
                name: row.2,
                description: row.3,
                parent_id: row.4,
                position: row.5,
                share_enabled: true,
                created_at: row.6,
                updated_at: row.7,
            },
            row.1, // user_id
        ))
    }
}
