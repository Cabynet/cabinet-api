pub mod queue;
pub mod enrichment;
pub mod thumbnail;

use std::sync::Arc;
use tokio::sync::Semaphore;
use crate::config::AppConfig;
use crate::infra::r2::R2Service;
use sqlx::PgPool;
use redis::aio::ConnectionManager;

pub struct WorkerEngine {
    pub pool: PgPool,
    pub redis: ConnectionManager,
    pub r2: R2Service,
    pub interactive_semaphore: Arc<Semaphore>,
    pub bulk_semaphore: Arc<Semaphore>,
    pub image_semaphore: Arc<Semaphore>,
}

impl WorkerEngine {
    pub fn new(
        pool: PgPool,
        redis: ConnectionManager,
        config: &AppConfig,
    ) -> Self {
        Self {
            pool,
            redis,
            r2: R2Service::new(config),
            interactive_semaphore: Arc::new(Semaphore::new(config.interactive_worker_concurrency)),
            bulk_semaphore: Arc::new(Semaphore::new(config.bulk_worker_concurrency)),
            image_semaphore: Arc::new(Semaphore::new(config.image_resize_concurrency)),
        }
    }
}
