use chrono::{DateTime, Utc};
use redis::AsyncCommands;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{error, info};
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::bookmarks::normalizer::normalize_url;
use crate::modules::folders::service::FoldersService;
use crate::worker::queue::{EnrichmentJob, QUEUE_BULK};
use super::dto::TaskStatusDto;
use super::export::{generate_netscape_html, BookmarkExportItem};
use super::parser::NetscapeParser;

#[derive(Clone)]
pub struct TasksService {
    pub pool: PgPool,
    pub redis: Option<redis::aio::ConnectionManager>,
}

impl TasksService {
    pub fn new(pool: PgPool, redis: Option<redis::aio::ConnectionManager>) -> Self {
        Self { pool, redis }
    }

    pub async fn get_task_status(
        &self,
        user_id: Uuid,
        task_id: Uuid,
    ) -> Result<TaskStatusDto, AppError> {
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                String,
                i32,
                i32,
                i32,
                Option<String>,
                DateTime<Utc>,
                Option<DateTime<Utc>>,
            ),
        >(
            r#"
            SELECT id, kind, status, total_items, processed_items, failed_items, error_message, created_at, finished_at
            FROM background_tasks
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(task_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Task not found".to_string()))?;

        Ok(TaskStatusDto {
            id: row.0,
            kind: row.1,
            status: row.2,
            total_items: row.3,
            processed_items: row.4,
            failed_items: row.5,
            error_message: row.6,
            created_at: row.7,
            finished_at: row.8,
        })
    }

    pub async fn import_html(
        &self,
        user_id: Uuid,
        html_content: &str,
    ) -> Result<TaskStatusDto, AppError> {
        let doc = NetscapeParser::parse(html_content);
        let total = doc.bookmarks.len() as i32;

        let task_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO background_tasks (id, user_id, kind, status, total_items, processed_items, failed_items, created_at)
            VALUES ($1, $2, 'IMPORT_HTML', 'PROCESSING', $3, 0, 0, NOW())
            "#,
        )
        .bind(task_id)
        .bind(user_id)
        .bind(total)
        .execute(&self.pool)
        .await?;

        // Process import (fast synchronous transaction for folders + bookmarks, enqueueing metadata to Redis)
        let pool = self.pool.clone();
        let mut redis = self.redis.clone();

        let mut folder_id_map: HashMap<usize, Uuid> = HashMap::new();

        // 1. Create folders preserving hierarchy
        for (idx, parsed_folder) in doc.folders.iter().enumerate() {
            let parent_id = parsed_folder.parent_idx.and_then(|p| folder_id_map.get(&p)).copied();
            let folder_id = Uuid::now_v7();

            let res = sqlx::query(
                r#"
                INSERT INTO folders (id, user_id, parent_id, name, position, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
                "#,
            )
            .bind(folder_id)
            .bind(user_id)
            .bind(parent_id)
            .bind(&parsed_folder.name)
            .bind(idx as i32)
            .execute(&pool)
            .await;

            if res.is_ok() {
                folder_id_map.insert(idx, folder_id);
            }
        }

        // 2. Insert bookmarks
        let mut processed = 0;
        let mut failed = 0;

        for bm in doc.bookmarks {
            let folder_id = bm.folder_idx.and_then(|idx| folder_id_map.get(&idx)).copied();
            let norm = match normalize_url(&bm.url) {
                Ok(n) => n,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };

            let bookmark_id = Uuid::now_v7();
            let created_at = bm
                .add_date
                .and_then(|ts| DateTime::from_timestamp(ts, 0))
                .unwrap_or_else(Utc::now);

            let insert_res = sqlx::query(
                r#"
                INSERT INTO bookmarks (
                    id, user_id, folder_id, item_type, raw_url, normalized_url, url_hash, normalized_domain,
                    title, user_edited_title, metadata_status, created_at, updated_at
                )
                VALUES ($1, $2, $3, 'LINK', $4, $5, $6, $7, $8, TRUE, 'PENDING', $9, NOW())
                "#,
            )
            .bind(bookmark_id)
            .bind(user_id)
            .bind(folder_id)
            .bind(&norm.raw_url)
            .bind(&norm.normalized_url)
            .bind(&norm.url_hash)
            .bind(&norm.domain)
            .bind(&bm.title)
            .bind(created_at)
            .execute(&pool)
            .await;

            match insert_res {
                Ok(_) => {
                    processed += 1;
                    if let Some(ref mut r) = redis {
                        let job = EnrichmentJob {
                            bookmark_id,
                            user_id,
                            url: norm.raw_url,
                            url_hash: norm.url_hash,
                            attempt: 0,
                        };
                        if let Ok(s) = serde_json::to_string(&job) {
                            let _: Result<(), redis::RedisError> = r.lpush(QUEUE_BULK, s).await;
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Failed to insert imported bookmark");
                    failed += 1;
                }
            }
        }

        // 3. Mark task completed
        sqlx::query(
            r#"
            UPDATE background_tasks
            SET status = 'COMPLETED', processed_items = $2, failed_items = $3, finished_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(task_id)
        .bind(processed)
        .bind(failed)
        .execute(&pool)
        .await?;

        info!(
            task_id = %task_id,
            processed = processed,
            failed = failed,
            "Bookmark HTML import completed successfully"
        );

        self.get_task_status(user_id, task_id).await
    }

    pub async fn export_html(&self, user_id: Uuid) -> Result<String, AppError> {
        let folders_service = FoldersService::new(self.pool.clone());
        let folders = folders_service.list_folders(user_id).await?;

        let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<Uuid>, DateTime<Utc>)>(
            r#"
            SELECT id, title, raw_url, folder_id, created_at
            FROM bookmarks
            WHERE user_id = $1
              AND deleted_at IS NULL
              AND is_archived = FALSE
              AND raw_url IS NOT NULL
            ORDER BY position ASC, created_at ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let bookmarks: Vec<BookmarkExportItem> = rows
            .into_iter()
            .map(|r| BookmarkExportItem {
                id: r.0,
                title: r.1,
                raw_url: r.2,
                folder_id: r.3,
                created_at: r.4,
            })
            .collect();

        Ok(generate_netscape_html(&folders, &bookmarks))
    }
}
