use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::infra::r2::R2Service;
use super::dto::{ConfirmUploadResponse, PresignUploadRequest, PresignUploadResponse};

#[derive(Clone)]
pub struct MediaService {
    pub pool: PgPool,
    pub r2: R2Service,
}

impl MediaService {
    pub fn new(pool: PgPool, r2: R2Service) -> Self {
        Self { pool, r2 }
    }

    pub async fn presign_upload(
        &self,
        user_id: Uuid,
        req: PresignUploadRequest,
    ) -> Result<PresignUploadResponse, AppError> {
        let max_file_size = 50 * 1024 * 1024; // 50 MB
        if req.size_bytes <= 0 || req.size_bytes > max_file_size {
            return Err(AppError::BadRequest(
                "File size must be between 1 byte and 50 MB".to_string(),
            ));
        }

        let mime = req.mime_type.to_lowercase();
        let is_allowed_mime = mime.starts_with("image/")
            || mime == "application/pdf"
            || mime == "text/plain"
            || mime == "application/json";

        if !is_allowed_mime {
            return Err(AppError::BadRequest(format!(
                "Unsupported media MIME type: {}",
                req.mime_type
            )));
        }

        // Check user quota
        let user_storage: (i64, i64) = sqlx::query_as(
            "SELECT storage_used_bytes, storage_quota_bytes FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

        if user_storage.0 + req.size_bytes > user_storage.1 {
            return Err(AppError::QuotaExceeded);
        }

        let asset_id = Uuid::now_v7();
        let clean_filename: String = req
            .filename
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
            .collect();

        let object_key = format!("uploads/{}/{}/{}", user_id, asset_id, clean_filename);
        let expires_seconds = 600; // 10 minutes

        let upload_url = self.r2.generate_presigned_put_url(&object_key, &mime, expires_seconds)?;
        let public_url = self.r2.get_public_url(&object_key);

        sqlx::query(
            r#"
            INSERT INTO media_assets (id, user_id, object_key, mime_type, size_bytes, status, created_at)
            VALUES ($1, $2, $3, $4, $5, 'PENDING_UPLOAD', NOW())
            "#,
        )
        .bind(asset_id)
        .bind(user_id)
        .bind(&object_key)
        .bind(&mime)
        .bind(req.size_bytes)
        .execute(&self.pool)
        .await?;

        Ok(PresignUploadResponse {
            asset_id,
            upload_url,
            public_url,
            object_key,
            expires_seconds,
        })
    }

    pub async fn confirm_upload(
        &self,
        user_id: Uuid,
        asset_id: Uuid,
    ) -> Result<ConfirmUploadResponse, AppError> {
        let mut tx = self.pool.begin().await?;

        let asset: (Uuid, i64, String, String) = sqlx::query_as(
            r#"
            SELECT id, size_bytes, object_key, status
            FROM media_assets
            WHERE id = $1 AND user_id = $2
            FOR UPDATE
            "#,
        )
        .bind(asset_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("Media asset not found".to_string()))?;

        let public_url = self.r2.get_public_url(&asset.2);

        if asset.3 == "READY" {
            tx.commit().await?;
            return Ok(ConfirmUploadResponse {
                asset_id,
                status: "READY".to_string(),
                public_url,
            });
        }

        sqlx::query(
            "UPDATE media_assets SET status = 'READY' WHERE id = $1",
        )
        .bind(asset_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE users SET storage_used_bytes = storage_used_bytes + $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(asset.1)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(ConfirmUploadResponse {
            asset_id,
            status: "READY".to_string(),
            public_url,
        })
    }
}
