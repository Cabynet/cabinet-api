use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::infra::http_client::SsrfSafeClient;
use super::enrichment::{ExtractedMetadata, MetadataExtractor};

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
    pub redis: redis::aio::ConnectionManager,
    pub extractor: MetadataExtractor,
}

impl WorkerProcessor {
    pub fn new(pool: PgPool, redis: redis::aio::ConnectionManager) -> Result<Self, anyhow::Error> {
        let http_client = SsrfSafeClient::new()?;
        let extractor = MetadataExtractor::new(http_client);
        Ok(Self {
            pool,
            redis,
            extractor,
        })
    }

    pub async fn process_job(&mut self, job: EnrichmentJob) {
        let hash_hex = hex::encode(&job.url_hash);
        let cache_key = format!("meta:cache:{}", hash_hex);

        info!(
            bookmark_id = %job.bookmark_id,
            url = %job.url,
            attempt = job.attempt,
            "Processing URL metadata enrichment job"
        );

        // 1. Check Redis cache for deduplication
        let cached_meta: Option<String> = self.redis.get(&cache_key).await.ok();
        let metadata = if let Some(cached_str) = cached_meta {
            serde_json::from_str::<ExtractedMetadata>(&cached_str).ok()
        } else {
            None
        };

        let final_meta = match metadata {
            Some(m) => {
                info!(url = %job.url, "Loaded metadata from Redis cache");
                Ok(m)
            }
            None => {
                let fetch_res = self.extractor.fetch_and_extract(&job.url).await;
                if let Ok(ref meta) = fetch_res {
                    if let Ok(json_str) = serde_json::to_string(meta) {
                        let _: Result<(), redis::RedisError> =
                            self.redis.set_ex(&cache_key, json_str, 86400).await;
                    }
                }
                fetch_res
            }
        };

        // 2. Database update: Fill-Missing, Never Overwrite User Intent
        match final_meta {
            Ok(meta) => {
                let res = sqlx::query(
                    r#"
                    UPDATE bookmarks
                    SET
                        title = CASE WHEN user_edited_title THEN title ELSE COALESCE($2, title) END,
                        description = CASE WHEN user_edited_desc THEN description ELSE COALESCE($3, description) END,
                        thumbnail_url = COALESCE(thumbnail_url, $4),
                        favicon_url = COALESCE(favicon_url, $5),
                        site_name = COALESCE(site_name, $6),
                        metadata_status = 'COMPLETED',
                        metadata_error = NULL,
                        last_enriched_at = NOW(),
                        updated_at = NOW()
                    WHERE id = $1 AND deleted_at IS NULL
                    "#,
                )
                .bind(job.bookmark_id)
                .bind(meta.title)
                .bind(meta.description)
                .bind(meta.image_url)
                .bind(meta.favicon_url)
                .bind(meta.site_name)
                .execute(&self.pool)
                .await;

                if let Err(e) = res {
                    error!(error = ?e, "Failed to update bookmark with enriched metadata");
                } else {
                    info!(bookmark_id = %job.bookmark_id, "Metadata enrichment completed successfully");
                }
            }
            Err(e) => {
                let err_msg = e.to_string();
                warn!(
                    bookmark_id = %job.bookmark_id,
                    error = %err_msg,
                    "Enrichment extraction failed"
                );

                let res = sqlx::query(
                    r#"
                    UPDATE bookmarks
                    SET
                        metadata_status = 'FAILED',
                        metadata_error = $2,
                        last_enriched_at = NOW(),
                        updated_at = NOW()
                    WHERE id = $1 AND deleted_at IS NULL
                    "#,
                )
                .bind(job.bookmark_id)
                .bind(err_msg.chars().take(280).collect::<String>())
                .execute(&self.pool)
                .await;

                if let Err(db_err) = res {
                    error!(error = ?db_err, "Failed to record enrichment error in DB");
                }
            }
        }
    }

    /// Single poll step: pops from queue:interactive first, then queue:bulk
    pub async fn poll_and_process_one(&mut self) -> bool {
        // Priority 1: Interactive Queue (Real-time saves)
        let job_str: Option<String> = self.redis.rpop(QUEUE_INTERACTIVE, None).await.ok().flatten();

        let job_str = match job_str {
            Some(j) => Some(j),
            // Priority 2: Bulk Queue (Imports & background sweeps)
            None => self.redis.rpop(QUEUE_BULK, None).await.ok().flatten(),
        };

        if let Some(s) = job_str {
            if let Ok(job) = serde_json::from_str::<EnrichmentJob>(&s) {
                self.process_job(job).await;
                return true;
            }
        }

        false
    }
}

pub fn spawn_worker_loop(
    pool: PgPool,
    redis: redis::aio::ConnectionManager,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let processor_res = WorkerProcessor::new(pool, redis);
        let mut processor = match processor_res {
            Ok(p) => p,
            Err(e) => {
                error!(error = ?e, "Worker failed to initialize HTTP client");
                return;
            }
        };

        info!("Background worker loop started listening to Redis queues");

        // Startup recovery: enqueue any existing PENDING bookmarks into Redis
        let pending_res = sqlx::query_as::<_, (Uuid, Uuid, Option<String>, Option<Vec<u8>>)>(
            r#"
            SELECT id, user_id, raw_url, url_hash
            FROM bookmarks
            WHERE metadata_status = 'PENDING' AND raw_url IS NOT NULL AND deleted_at IS NULL
            ORDER BY created_at DESC
            LIMIT 500
            "#,
        )
        .fetch_all(&processor.pool)
        .await;

        if let Ok(rows) = pending_res {
            if !rows.is_empty() {
                info!("Startup recovery: found {} pending bookmarks, enqueuing to Redis", rows.len());
                for (id, user_id, raw_url, url_hash) in rows {
                    if let (Some(u), Some(h)) = (raw_url, url_hash) {
                        let job = EnrichmentJob {
                            bookmark_id: id,
                            user_id,
                            url: u,
                            url_hash: h,
                            attempt: 0,
                        };
                        if let Ok(job_str) = serde_json::to_string(&job) {
                            let _: Result<(), redis::RedisError> =
                                processor.redis.lpush(QUEUE_INTERACTIVE, job_str).await;
                        }
                    }
                }
            }
        }

        loop {
            let processed = processor.poll_and_process_one().await;
            if !processed {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    })
}
