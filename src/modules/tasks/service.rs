use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    dto::TaskStatusDto,
    export::{generate_netscape_html, BookmarkExportItem},
    parser::NetscapeParser,
};
use crate::errors::AppError;
use crate::modules::bookmarks::normalizer::normalize_url;
use crate::modules::folders::service::FoldersService;

type TaskRow = (Uuid, String, Option<String>, Option<Uuid>, DateTime<Utc>);

#[derive(Clone)]
pub struct TasksService {
    pub pool: PgPool,
    pub redis: Option<redis::aio::ConnectionManager>,
}

static TASKS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn stable_id(task: Uuid, kind: &str, index: usize) -> Uuid {
    let mut h = Sha256::new();
    h.update(task.as_bytes());
    h.update(kind.as_bytes());
    h.update((index as u64).to_be_bytes());
    let digest = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 15) | 0x80;
    bytes[8] = (bytes[8] & 63) | 0x80;
    Uuid::from_bytes(bytes)
}

impl TasksService {
    pub fn new(pool: PgPool, redis: Option<redis::aio::ConnectionManager>) -> Self {
        Self { pool, redis }
    }

    pub async fn get_task_status(&self, user: Uuid, id: Uuid) -> Result<TaskStatusDto, AppError> {
        let value: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(t) - 'payload' FROM background_tasks t WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Task not found".into()))?;

        serde_json::from_value(crate::infra::camel::camelize(value)).map_err(|e| AppError::Internal(e.into()))
    }

    pub async fn import_html(&self, user: Uuid, html: &str) -> Result<TaskStatusDto, AppError> {
        if html.len() > 1_000_000 {
            return Err(AppError::BadRequest("Import files are limited to 1 MB".into()));
        }

        let parsed = NetscapeParser::parse(html);
        if parsed.bookmarks.is_empty() || parsed.bookmarks.len() + parsed.folders.len() > 10000 {
            return Err(AppError::BadRequest(
                "Import between 1 and 10,000 bookmark/folder entries".into(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user)
            .execute(&mut *tx)
            .await?;

        let active: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM background_tasks
                WHERE user_id = $1 AND kind = 'IMPORT_HTML' AND status IN ('PENDING', 'RUNNING')
            )
            "#,
        )
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;

        if active {
            return Err(AppError::Conflict("An import is already active; check Activity".into()));
        }

        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO background_tasks (id, user_id, kind, status, total_items, payload)
            VALUES ($1, $2, 'IMPORT_HTML', 'PENDING', $3, $4)
            "#,
        )
        .bind(id)
        .bind(user)
        .bind(parsed.bookmarks.len() as i32)
        .bind(html)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        let task = self.get_task_status(user, id).await?;
        self.start_task(id);
        Ok(task)
    }

    pub fn start_task(&self, id: Uuid) {
        let Ok(permit) = TASKS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
        else {
            return;
        };

        let service = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let claim = sqlx::query_as::<_, (String, Uuid, i32)>(
                r#"
                UPDATE background_tasks
                SET status = 'RUNNING',
                    attempt = attempt + 1,
                    processed_items = 0,
                    failed_items = 0,
                    updated_at = now()
                WHERE id = $1 AND status = 'PENDING'
                RETURNING kind, user_id, attempt
                "#,
            )
            .bind(id)
            .fetch_optional(&service.pool)
            .await;

            let (kind, user, attempt) = match claim {
                Ok(Some(row)) => row,
                Ok(None) => return,
                Err(error) => {
                    tracing::error!(error = ?error, "Task claim failed");
                    return;
                }
            };

            let heartbeat = spawn_heartbeat(service.pool.clone(), id, attempt, std::time::Duration::from_secs(30));
            let result = match kind.as_str() {
                "IMPORT_HTML" => service.process_import(id, user, attempt).await,
                "LINK_CHECK" => service.process_health(id, user, attempt).await,
                _ => Err(AppError::BadRequest("Unknown task kind".into())),
            };

            heartbeat.abort();
            if let Err(error) = result {
                tracing::error!(task_id = %id, error = ?error, "Background task failed");
                let error_msg: String = error.to_string().chars().take(280).collect();
                let _ = sqlx::query(
                    r#"
                    UPDATE background_tasks
                    SET status = 'FAILED',
                        error_message = $2,
                        updated_at = now(),
                        finished_at = now()
                    WHERE id = $1 AND status = 'RUNNING' AND attempt = $3
                    "#,
                )
                .bind(id)
                .bind(error_msg)
                .bind(attempt)
                .execute(&service.pool)
                .await;
            }
        });
    }

    async fn process_import(&self, id: Uuid, user: Uuid, attempt: i32) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user)
            .execute(&mut *tx)
            .await?;

        let Some(html) = sqlx::query_scalar::<_, String>(
            r#"
            SELECT payload
            FROM background_tasks
            WHERE id = $1 AND status = 'RUNNING' AND payload IS NOT NULL AND attempt = $2
            "#,
        )
        .bind(id)
        .bind(attempt)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(());
        };

        let doc = NetscapeParser::parse(&html);
        let mut folders = std::collections::HashMap::new();

        for (index, folder) in doc.folders.iter().enumerate() {
            let folder_id = stable_id(id, "folder", index);
            let parent = folder.parent_idx.and_then(|p| folders.get(&p)).copied();
            let name: String = folder.name.chars().take(120).collect();

            sqlx::query(
                r#"
                INSERT INTO folders (id, user_id, parent_id, name, position)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (id) DO NOTHING
                "#,
            )
            .bind(folder_id)
            .bind(user)
            .bind(parent)
            .bind(name)
            .bind(index as i32)
            .execute(&mut *tx)
            .await?;

            folders.insert(index, folder_id);
        }

        let mut processed = 0;
        let mut failed = 0;

        // (id, folder, normalized, created, title) per valid item; inserted in chunks with unnest.
        let mut rows = Vec::with_capacity(doc.bookmarks.len());
        for (index, item) in doc.bookmarks.iter().enumerate() {
            let Ok(norm) = normalize_url(&item.url) else {
                failed += 1;
                continue;
            };
            let folder = item.folder_idx.and_then(|i| folders.get(&i)).copied();
            let created = item
                .add_date
                .and_then(|t| DateTime::from_timestamp(t, 0))
                .unwrap_or_else(Utc::now);
            rows.push((stable_id(id, "bookmark", index), folder, norm, created, item.title.clone()));
        }

        for chunk in rows.chunks(500) {
            sqlx::query(
                r#"
                INSERT INTO bookmarks (
                    id, user_id, folder_id, item_type, raw_url, normalized_url,
                    url_hash, normalized_domain, title, user_edited_title, metadata_status, created_at
                )
                SELECT i, $1, f, 'LINK', r, n, h, d, t, true, 'PENDING', c
                FROM unnest($2::uuid[], $3::uuid[], $4::text[], $5::text[], $6::bytea[], $7::text[], $8::text[], $9::timestamptz[])
                     AS v(i, f, r, n, h, d, t, c)
                ON CONFLICT (id) DO NOTHING
                "#,
            )
            .bind(user)
            .bind(chunk.iter().map(|r| r.0).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.1).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.2.raw_url.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.2.normalized_url.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.2.url_hash.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.2.domain.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.4.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|r| r.3).collect::<Vec<_>>())
            .execute(&mut *tx)
            .await?;
            processed += chunk.len() as i32;
        }

        sqlx::query(
            r#"
            UPDATE background_tasks
            SET status = 'COMPLETED',
                processed_items = $2,
                failed_items = $3,
                error_message = NULL,
                payload = NULL,
                updated_at = now(),
                finished_at = now()
            WHERE id = $1 AND attempt = $4 AND status = 'RUNNING'
            "#,
        )
        .bind(id)
        .bind(processed)
        .bind(failed)
        .bind(attempt)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        crate::worker::queue::wake_workers();
        Ok(())
    }

    async fn process_health(&self, id: Uuid, user: Uuid, attempt: i32) -> Result<(), AppError> {
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM bookmarks WHERE user_id = $1 AND raw_url IS NOT NULL AND deleted_at IS NULL",
        )
        .bind(user)
        .fetch_one(&self.pool)
        .await?;

        sqlx::query("UPDATE background_tasks SET total_items = $2 WHERE id = $1 AND attempt = $3 AND status = 'RUNNING'")
            .bind(id)
            .bind(total as i32)
            .bind(attempt)
            .execute(&self.pool)
            .await?;

        let mut cursor = Uuid::nil();
        loop {
            let ids: Vec<Uuid> = sqlx::query_scalar(
                r#"
                SELECT id FROM bookmarks
                WHERE user_id = $1 AND raw_url IS NOT NULL AND deleted_at IS NULL AND id > $2
                ORDER BY id
                LIMIT 100
                "#,
            )
            .bind(user)
            .bind(cursor)
            .fetch_all(&self.pool)
            .await?;

            if ids.is_empty() {
                break;
            }
            cursor = *ids.last().unwrap();

            for chunk in ids.chunks(4) {
                let mut jobs = tokio::task::JoinSet::new();
                for bookmark in chunk {
                    let svc = crate::modules::bookmarks::service::BookmarksService::new(
                        self.pool.clone(),
                        None,
                    );
                    let bookmark = *bookmark;
                    jobs.spawn(async move { svc.check_bookmark_health(user, bookmark).await.is_err() });
                }

                while let Some(result) = jobs.join_next().await {
                    let failed = result.unwrap_or(true);
                    sqlx::query(
                        r#"
                        UPDATE background_tasks
                        SET processed_items = processed_items + 1,
                            total_items = greatest(total_items, processed_items + 1),
                            failed_items = failed_items + $2,
                            updated_at = now()
                        WHERE id = $1 AND status = 'RUNNING' AND attempt = $3
                        "#,
                    )
                    .bind(id)
                    .bind(if failed { 1 } else { 0 })
                    .bind(attempt)
                    .execute(&self.pool)
                    .await?;
                }
            }
        }

        sqlx::query(
            r#"
            UPDATE background_tasks
            SET status = 'COMPLETED',
                finished_at = now(),
                updated_at = now(),
                error_message = NULL
            WHERE id = $1 AND status = 'RUNNING' AND attempt = $2
            "#,
        )
        .bind(id)
        .bind(attempt)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn export_html(&self, user: Uuid) -> Result<String, AppError> {
        let folders = FoldersService::new(self.pool.clone())
            .list_folders(user)
            .await?
            .into_iter()
            .filter(|f| f.access == "OWNER")
            .collect::<Vec<_>>();

        let rows: Vec<TaskRow> = sqlx::query_as(
            r#"
            SELECT id, title, raw_url, folder_id, created_at
            FROM bookmarks
            WHERE user_id = $1 AND deleted_at IS NULL AND is_archived = false AND raw_url IS NOT NULL
            ORDER BY position, created_at, id
            "#,
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;

        let items = rows
            .into_iter()
            .map(|r| BookmarkExportItem {
                id: r.0,
                title: r.1,
                raw_url: r.2,
                folder_id: r.3,
                created_at: r.4,
            })
            .collect::<Vec<_>>();

        Ok(generate_netscape_html(&folders, &items))
    }
}

/// Keeps `updated_at` fresh while a task runs so recovery never re-queues a live task.
pub fn spawn_heartbeat(
    pool: PgPool,
    id: Uuid,
    attempt: i32,
    period: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tick.tick().await;
        loop {
            tick.tick().await;
            let _ = sqlx::query(
                "UPDATE background_tasks SET updated_at = now() WHERE id = $1 AND attempt = $2 AND status = 'RUNNING'",
            )
            .bind(id)
            .bind(attempt)
            .execute(&pool)
            .await;
        }
    })
}

pub fn spawn_recovery(pool: PgPool) {
    tokio::spawn(async move {
        let service = TasksService::new(pool, None);
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            tick.tick().await;
            let _ = sqlx::query(
                r#"
                UPDATE background_tasks
                SET status = 'PENDING', updated_at = now()
                WHERE kind IN ('IMPORT_HTML', 'LINK_CHECK')
                  AND status = 'RUNNING'
                  AND attempt < 3
                  AND updated_at < now() - interval '5 minutes'
                "#,
            )
            .execute(&service.pool)
            .await;

            // Give up after 3 attempts instead of re-running a stuck task forever.
            let _ = sqlx::query(
                r#"
                UPDATE background_tasks
                SET status = 'FAILED', updated_at = now()
                WHERE kind IN ('IMPORT_HTML', 'LINK_CHECK')
                  AND status = 'RUNNING'
                  AND attempt >= 3
                  AND updated_at < now() - interval '5 minutes'
                "#,
            )
            .execute(&service.pool)
            .await;

            if let Ok(ids) = sqlx::query_scalar::<_, Uuid>(
                r#"
                SELECT id FROM background_tasks
                WHERE kind IN ('IMPORT_HTML', 'LINK_CHECK')
                  AND status = 'PENDING'
                ORDER BY created_at
                LIMIT 20
                "#,
            )
            .fetch_all(&service.pool)
            .await
            {
                for id in ids {
                    service.start_task(id);
                }
            }
        }
    });
}
