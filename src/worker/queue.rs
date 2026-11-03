use std::sync::OnceLock;
use std::time::Duration;

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::Notify;
use tracing::{error, info};
use uuid::Uuid;

use super::enrichment::{ExtractedMetadata, MetadataExtractor};
use crate::infra::http_client::SsrfSafeClient;

static WAKE: OnceLock<Notify> = OnceLock::new();

pub fn wake_workers() {
    WAKE.get_or_init(Notify::new).notify_one();
}

pub const QUEUE_INTERACTIVE: &str = "cabinet:queue:interactive";
pub const QUEUE_BULK: &str = "cabinet:queue:bulk";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EnrichmentJob {
    pub bookmark_id: Uuid,
    pub user_id: Uuid,
    pub url: String,
    pub url_hash: Vec<u8>,
    pub attempt: u32,
}

pub struct WorkerProcessor {
    pub pool: PgPool,
    pub redis: Option<redis::aio::ConnectionManager>,
    pub extractor: MetadataExtractor,
}

impl WorkerProcessor {
    pub fn new(
        pool: PgPool,
        redis: impl Into<Option<redis::aio::ConnectionManager>>,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self {
            pool,
            redis: redis.into(),
            extractor: MetadataExtractor::new(SsrfSafeClient::new()?),
        })
    }

    pub async fn process_job(&mut self, job: EnrichmentJob) {
        let claim = sqlx::query_scalar::<_, i32>(
            r#"
            UPDATE bookmarks
            SET metadata_status = 'PROCESSING',
                metadata_attempts = metadata_attempts + 1,
                metadata_started_at = now()
            WHERE id = $1
              AND user_id = $2
              AND raw_url = $3
              AND deleted_at IS NULL
              AND metadata_status = 'PENDING'
              AND metadata_attempts < 3
              AND (metadata_started_at IS NULL OR metadata_started_at <= now())
            RETURNING metadata_attempts
            "#,
        )
        .bind(job.bookmark_id)
        .bind(job.user_id)
        .bind(&job.url)
        .fetch_optional(&self.pool)
        .await;

        let attempt = match claim {
            Ok(Some(n)) => n,
            Ok(None) => return,
            Err(e) => {
                error!(error = ?e, "Metadata claim failed");
                return;
            }
        };

        let key = format!("meta:cache:{}", hex::encode(&job.url_hash));
        let cached: Option<String> = if let Some(redis) = &mut self.redis {
            redis.get(&key).await.ok()
        } else {
            None
        };

        let metadata = cached.and_then(|s| serde_json::from_str::<ExtractedMetadata>(&s).ok());
        let result = match metadata {
            Some(m) => Ok(m),
            None => self.extractor.fetch_and_extract(&job.url).await,
        };

        let write = match result {
            Ok(meta) => {
                if let (Some(redis), Ok(encoded)) = (&mut self.redis, serde_json::to_string(&meta)) {
                    let _: Result<(), _> = redis.set_ex(&key, encoded, 86400).await;
                }

                sqlx::query(
                    r#"
                    UPDATE bookmarks
                    SET title = CASE WHEN user_edited_title THEN title ELSE coalesce($4, title) END,
                        description = CASE WHEN user_edited_desc THEN description ELSE coalesce($5, description) END,
                        thumbnail_url = coalesce(thumbnail_url, $6),
                        favicon_url = coalesce(favicon_url, $7),
                        site_name = coalesce(site_name, $8),
                        metadata_status = 'COMPLETED',
                        metadata_error = NULL,
                        last_enriched_at = now(),
                        updated_at = now()
                    WHERE id = $1
                      AND user_id = $2
                      AND raw_url = $3
                      AND deleted_at IS NULL
                      AND metadata_status = 'PROCESSING'
                      AND metadata_attempts = $9
                    "#,
                )
                .bind(job.bookmark_id)
                .bind(job.user_id)
                .bind(&job.url)
                .bind(meta.title)
                .bind(meta.description)
                .bind(meta.image_url)
                .bind(meta.favicon_url)
                .bind(meta.site_name)
                .bind(attempt)
                .execute(&self.pool)
                .await
            }
            Err(e) => {
                let err_msg: String = e.to_string().chars().take(280).collect();
                sqlx::query(
                    r#"
                    UPDATE bookmarks
                    SET metadata_status = CASE WHEN $4 < 3 THEN 'PENDING' ELSE 'FAILED' END,
                        metadata_error = $5,
                        metadata_started_at = now() + make_interval(secs => power(2, $4)::int),
                        last_enriched_at = now(),
                        updated_at = now()
                    WHERE id = $1
                      AND user_id = $2
                      AND raw_url = $3
                      AND deleted_at IS NULL
                      AND metadata_status = 'PROCESSING'
                      AND metadata_attempts = $4
                    "#,
                )
                .bind(job.bookmark_id)
                .bind(job.user_id)
                .bind(&job.url)
                .bind(attempt)
                .bind(err_msg)
                .execute(&self.pool)
                .await
            }
        };

        if let Err(e) = write {
            error!(error = ?e, "Metadata result could not be saved; stale claim recovery will retry");
        }
    }

    pub async fn poll_and_process_one(&mut self) -> bool {
        if let Some(redis) = &mut self.redis {
            let interactive: Option<String> = redis.rpop(QUEUE_INTERACTIVE, None).await.ok().flatten();
            let next = if interactive.is_some() {
                interactive
            } else {
                redis.rpop(QUEUE_BULK, None).await.ok().flatten()
            };

            if let Some(encoded) = next {
                if let Ok(job) = serde_json::from_str(&encoded) {
                    self.process_job(job).await;
                    return true;
                }
            }
        }

        // Redis is a wake-up hint; durable database rows recover lost jobs and outages.
        let row = sqlx::query_as::<_, (Uuid, Uuid, String, Vec<u8>)>(
            r#"
            SELECT id, user_id, raw_url, url_hash
            FROM bookmarks
            WHERE metadata_status = 'PENDING'
              AND item_type = 'LINK'
              AND raw_url IS NOT NULL
              AND url_hash IS NOT NULL
              AND deleted_at IS NULL
              AND metadata_attempts < 3
              AND (metadata_started_at IS NULL OR metadata_started_at <= now())
            ORDER BY created_at
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await;

        if let Ok(Some((bookmark_id, user_id, url, url_hash))) = row {
            self.process_job(EnrichmentJob {
                bookmark_id,
                user_id,
                url,
                url_hash,
                attempt: 0,
            })
            .await;
            return true;
        }

        false
    }
}

async fn run_worker(pool: PgPool, redis: Option<redis::aio::ConnectionManager>) {
    let mut processor = match WorkerProcessor::new(pool, redis) {
        Ok(p) => p,
        Err(e) => {
            error!(error = ?e, "Worker initialization failed");
            return;
        }
    };

    loop {
        if !processor.poll_and_process_one().await {
            tokio::select! {
                _ = WAKE.get_or_init(Notify::new).notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }
}

/// Waits for a worker to end (it only ends by panic or failed init) and returns whether to respawn it.
async fn reap_worker(workers: &mut tokio::task::JoinSet<()>) -> bool {
    match workers.join_next().await {
        Some(Err(e)) => error!(error = ?e, "Metadata worker panicked; respawning"),
        Some(Ok(())) => error!("Metadata worker exited; respawning"),
        None => return false,
    }
    // Avoid a hot loop if the worker fails immediately every time.
    tokio::time::sleep(Duration::from_secs(1)).await;
    true
}

pub fn spawn_worker_loop(
    pool: PgPool,
    redis: impl Into<Option<redis::aio::ConnectionManager>>,
) -> tokio::task::JoinHandle<()> {
    let redis = redis.into();

    tokio::spawn(async move {
        let concurrency = crate::config::AppConfig::shared()
            .map(|c| c.interactive_worker_concurrency + c.bulk_worker_concurrency)
            .unwrap_or(4)
            .clamp(1, 16);

        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..concurrency {
            workers.spawn(run_worker(pool.clone(), redis.clone()));
        }

        info!(concurrency, "Metadata workers started");

        let mut recovery = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = recovery.tick() => {
                    let _ = sqlx::query(
                        r#"
                        UPDATE bookmarks
                        SET metadata_status = CASE WHEN metadata_attempts < 3 THEN 'PENDING' ELSE 'FAILED' END,
                            metadata_error = 'Worker interrupted; recovery scheduled',
                            metadata_started_at = NULL
                        WHERE metadata_status = 'PROCESSING'
                          AND metadata_started_at < now() - interval '2 minutes'
                        "#,
                    )
                    .execute(&pool)
                    .await;
                }
                true = reap_worker(&mut workers) => {
                    workers.spawn(run_worker(pool.clone(), redis.clone()));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicked_worker_is_reaped_for_respawn() {
        let mut workers = tokio::task::JoinSet::new();
        assert!(!reap_worker(&mut workers).await, "no workers means nothing to respawn");
        workers.spawn(async { panic!("worker bug") });
        assert!(reap_worker(&mut workers).await);
    }
}
