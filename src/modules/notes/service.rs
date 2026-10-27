use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use super::dto::{NoteResponse, UpdateNoteRequest};

#[derive(Clone)]
pub struct NotesService {
    pub pool: PgPool,
}

impl NotesService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_note(&self, user_id: Uuid, bookmark_id: Uuid) -> Result<NoteResponse, AppError> {
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                i32,
                Option<Uuid>,
                Option<serde_json::Value>,
                Option<String>,
                DateTime<Utc>,
            ),
        >(
            r#"
            SELECT id, note_revision, last_note_save_id, note_document, notes_plain, updated_at
            FROM bookmarks
            WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Bookmark or note not found".to_string()))?;

        Ok(NoteResponse {
            id: row.0,
            revision: row.1,
            save_id: row.2,
            document: row.3,
            plain_text: row.4,
            updated_at: row.5,
        })
    }

    pub async fn update_note(
        &self,
        user_id: Uuid,
        bookmark_id: Uuid,
        req: UpdateNoteRequest,
    ) -> Result<NoteResponse, AppError> {
        // 1. Fetch current revision and last save ID
        let current = sqlx::query_as::<
            _,
            (
                i32,
                Option<Uuid>,
                Option<serde_json::Value>,
                Option<String>,
                DateTime<Utc>,
            ),
        >(
            r#"
            SELECT note_revision, last_note_save_id, note_document, notes_plain, updated_at
            FROM bookmarks
            WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Bookmark not found".to_string()))?;

        let (current_revision, last_save_id, cur_doc, cur_plain, cur_updated) = current;

        // 2. Check for identical save_id retry (idempotent autosave replay)
        if let (Some(req_save), Some(last_save)) = (req.save_id, last_save_id) {
            if req_save == last_save {
                return Ok(NoteResponse {
                    id: bookmark_id,
                    revision: current_revision,
                    save_id: Some(last_save),
                    document: cur_doc,
                    plain_text: cur_plain,
                    updated_at: cur_updated,
                });
            }
        }

        // 3. Optimistic concurrency check
        if current_revision != req.base_revision {
            return Err(AppError::Conflict(format!(
                "NOTE_CONFLICT: current revision is {}, but request was based on revision {}",
                current_revision, req.base_revision
            )));
        }

        // 4. Update note and bump revision atomically
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                i32,
                Option<Uuid>,
                Option<serde_json::Value>,
                Option<String>,
                DateTime<Utc>,
            ),
        >(
            r#"
            UPDATE bookmarks
            SET
                note_document = $3,
                notes_plain = $4,
                note_revision = note_revision + 1,
                last_note_save_id = $5,
                updated_at = NOW()
            WHERE id = $1 AND user_id = $2 AND note_revision = $6 AND deleted_at IS NULL
            RETURNING id, note_revision, last_note_save_id, note_document, notes_plain, updated_at
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .bind(req.document)
        .bind(req.plain_text)
        .bind(req.save_id)
        .bind(req.base_revision)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("Concurrent modification detected while saving note".to_string())
        })?;

        Ok(NoteResponse {
            id: row.0,
            revision: row.1,
            save_id: row.2,
            document: row.3,
            plain_text: row.4,
            updated_at: row.5,
        })
    }
}
