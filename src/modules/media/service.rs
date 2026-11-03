use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::infra::r2::R2Service;
use super::dto::{ConfirmUploadResponse, PresignUploadRequest, PresignUploadResponse};

/// Permanently deletes the user's trashed bookmarks (all, or only `only`), releases the quota of
/// uploaded files that nothing else references, and returns their object keys. Call
/// `delete_objects` with them after the transaction commits.
pub async fn purge_trashed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    only: Option<Vec<Uuid>>,
) -> Result<(u64, Vec<String>), AppError> {
    // Assets are linked through media_assets.bookmark_id (set when the bookmark is created). Another
    // user's copied bookmark points at the same URL without an asset row, so it still protects the object.
    let assets: Vec<(Uuid, String, i64)> = sqlx::query_as(
        r#"
        WITH purged AS (
            SELECT id FROM bookmarks
            WHERE user_id = $1 AND deleted_at IS NOT NULL AND ($2::uuid[] IS NULL OR id = ANY($2))
        )
        SELECT m.id, m.object_key, m.size_bytes FROM media_assets m
        WHERE m.user_id = $1 AND m.status = 'READY'
          AND m.bookmark_id IN (SELECT id FROM purged)
          AND NOT EXISTS (
            SELECT 1 FROM bookmarks o
            WHERE right(o.raw_url, length(m.object_key)) = m.object_key
              AND o.id NOT IN (SELECT id FROM purged))
        "#,
    )
    .bind(user_id)
    .bind(&only)
    .fetch_all(&mut **tx)
    .await?;

    let deleted = sqlx::query(
        "DELETE FROM bookmarks WHERE user_id = $1 AND deleted_at IS NOT NULL AND ($2::uuid[] IS NULL OR id = ANY($2))",
    )
    .bind(user_id)
    .bind(&only)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if !assets.is_empty() {
        let ids: Vec<Uuid> = assets.iter().map(|a| a.0).collect();
        let freed: i64 = assets.iter().map(|a| a.2).sum();
        sqlx::query("DELETE FROM media_assets WHERE id = ANY($1)").bind(&ids).execute(&mut **tx).await?;
        sqlx::query("UPDATE users SET storage_used_bytes = GREATEST(storage_used_bytes - $2, 0), updated_at = NOW() WHERE id = $1")
            .bind(user_id)
            .bind(freed)
            .execute(&mut **tx)
            .await?;
    }
    Ok((deleted, assets.into_iter().map(|a| a.1).collect()))
}

/// Best-effort R2 cleanup after the database commit; failures only leave an orphaned object.
pub async fn delete_objects(keys: Vec<String>) {
    let Ok(config) = crate::config::AppConfig::shared() else { return };
    let r2 = R2Service::new(config);
    for key in keys {
        if let Err(error) = r2.delete_object(&key).await {
            tracing::warn!(%key, %error, "Could not delete R2 object");
        }
    }
}

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
        let is_allowed_mime = matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/avif")
            || mime == "application/pdf"
            || mime == "text/plain"
            || mime == "application/json";

        if !is_allowed_mime {
            return Err(AppError::BadRequest(format!(
                "Unsupported media MIME type: {}",
                req.mime_type
            )));
        }

        let mut tx = self.pool.begin().await?;
        // Reserve quota under the account lock, including unexpired upload slots.
        let user_storage: (i64, i64) = sqlx::query_as(
            "SELECT storage_used_bytes, storage_quota_bytes FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;
        let pending: i64 = sqlx::query_scalar(
            r#"
            SELECT coalesce(sum(size_bytes), 0)::bigint
            FROM media_assets
            WHERE user_id = $1
              AND status = 'PENDING_UPLOAD'
              AND created_at > now() - interval '10 minutes'
            "#,
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if user_storage.0 + pending + req.size_bytes > user_storage.1 {
            return Err(AppError::QuotaExceeded);
        }

        let asset_id = Uuid::now_v7();
        let clean_filename: String = req
            .filename
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
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
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(PresignUploadResponse {
            asset_id,
            upload_url,
            public_url,
            object_key,
            expires_seconds,
        })
    }

    pub async fn upload_file(
        &self,
        user_id: Uuid,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<ConfirmUploadResponse, AppError> {
        let max_file_size = 50 * 1024 * 1024; // 50 MB
        let size_bytes = bytes.len() as i64;
        if size_bytes <= 0 || size_bytes > max_file_size {
            return Err(AppError::BadRequest(
                "File size must be between 1 byte and 50 MB".to_string(),
            ));
        }

        let mime = mime_type.to_lowercase();
        let is_allowed_mime = matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/avif" | "image/svg+xml")
            || mime == "application/pdf"
            || mime == "text/plain"
            || mime == "application/json";

        if !is_allowed_mime {
            return Err(AppError::BadRequest(format!(
                "Unsupported media MIME type: {}",
                mime_type
            )));
        }

        let asset_id = Uuid::now_v7();
        let clean_filename: String = filename
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
            .collect();

        let object_key = format!("uploads/{}/{}/{}", user_id, asset_id, clean_filename);

        // Upload before taking the account lock so a slow R2 call never holds a row lock or connection.
        let public_url = self.r2.upload_bytes(&object_key, &mime, bytes).await?;

        match self.register_upload(user_id, asset_id, &object_key, &mime, size_bytes).await {
            Ok(()) => Ok(ConfirmUploadResponse {
                asset_id,
                status: "READY".to_string(),
                public_url,
            }),
            Err(error) => {
                delete_objects(vec![object_key]).await;
                Err(error)
            }
        }
    }

    async fn register_upload(
        &self,
        user_id: Uuid,
        asset_id: Uuid,
        object_key: &str,
        mime: &str,
        size_bytes: i64,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let user_storage: (i64, i64) = sqlx::query_as(
            "SELECT storage_used_bytes, storage_quota_bytes FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

        if user_storage.0 + size_bytes > user_storage.1 {
            return Err(AppError::QuotaExceeded);
        }

        sqlx::query(
            r#"
            INSERT INTO media_assets (id, user_id, object_key, mime_type, size_bytes, status, created_at)
            VALUES ($1, $2, $3, $4, $5, 'READY', NOW())
            "#,
        )
        .bind(asset_id)
        .bind(user_id)
        .bind(object_key)
        .bind(mime)
        .bind(size_bytes)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE users SET storage_used_bytes = storage_used_bytes + $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(size_bytes)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
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

        self.r2.verify_object(&asset.2, asset.1).await?;
        let quota_ok: bool = sqlx::query_scalar(
            "SELECT storage_used_bytes + $2 <= storage_quota_bytes FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .bind(asset.1)
        .fetch_one(&mut *tx)
        .await?;
        if !quota_ok {
            return Err(AppError::QuotaExceeded);
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
