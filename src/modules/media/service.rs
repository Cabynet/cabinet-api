use sqlx::PgPool;
use crate::infra::r2::R2Service;

#[derive(Clone)]
pub struct MediaService {
    pub pool: PgPool,
    pub r2: R2Service,
}

impl MediaService {
    pub fn new(pool: PgPool, r2: R2Service) -> Self {
        Self { pool, r2 }
    }
}
