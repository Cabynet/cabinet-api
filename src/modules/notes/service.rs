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
            WHERE id = $1 AND (user_id = $2 OR EXISTS(SELECT 1 FROM folder_members m WHERE m.folder_id=bookmarks.folder_id AND m.user_id=$2) OR folder_id IN (SELECT id FROM folders WHERE user_id = $2)) AND deleted_at IS NULL
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
        let bookmarks = crate::modules::bookmarks::service::BookmarksService::new(self.pool.clone(), None);
        bookmarks.check_folder(user_id, req.folder_id).await?;
        let actor = user_id;
        let user_id = match bookmarks.get_bookmark(user_id, bookmark_id).await {
            Ok(item) => {
                if item.access == "viewer" { return Err(AppError::Forbidden("This note is read-only".into())); }
                if item.user_id != user_id && (req.set_folder.unwrap_or(false) || req.tags.is_some()) { return Err(AppError::Forbidden("Personal fields belong to the note owner".into())); }
                item.user_id
            }
            Err(AppError::NotFound(_)) if req.base_revision == 0 => user_id,
            Err(error) => return Err(error),
        };
        let mut tx = self.pool.begin().await?;
        if req.base_revision == 0 {
            sqlx::query("INSERT INTO bookmarks(id,user_id,item_type,title,folder_id,metadata_status) VALUES($1,$2,'NOTE',coalesce($3,'Untitled note'),$4,'COMPLETED') ON CONFLICT(id) DO NOTHING")
                .bind(bookmark_id).bind(user_id).bind(&req.title).bind(req.folder_id).execute(&mut *tx).await?;
        }
        // Serialize saves so duplicate save IDs and concurrent revisions are checked atomically.
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
            FOR UPDATE
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
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
                note_document = COALESCE($3, note_document),
                notes_plain = COALESCE($4, notes_plain),
                title = COALESCE($7, title),
                folder_id = CASE WHEN $8 THEN $9 ELSE folder_id END,
                note_revision = note_revision + 1,
                last_note_save_id = $5,
                updated_at = NOW()
            WHERE id = $1 AND user_id = $2 AND note_revision = $6 AND deleted_at IS NULL
              AND (user_id=$10 OR EXISTS(SELECT 1 FROM folder_members m WHERE m.folder_id=bookmarks.folder_id AND m.user_id=$10 AND m.role='EDITOR'))
            RETURNING id, note_revision, last_note_save_id, note_document, notes_plain, updated_at
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .bind(req.document)
        .bind(req.plain_text)
        .bind(req.save_id)
        .bind(req.base_revision)
        .bind(req.title)
        .bind(req.set_folder.unwrap_or(false))
        .bind(req.folder_id)
        .bind(actor)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("Concurrent modification detected while saving note".to_string())
        })?;

        if let Some(tags) = req.tags {
            crate::modules::bookmarks::service::BookmarksService::set_tags(&mut tx, user_id, bookmark_id, &tags, true).await?;
        }
        tx.commit().await?;
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
