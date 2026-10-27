use sqlx::PgPool;

#[derive(Clone)]
pub struct SearchService {
    pub pool: PgPool,
}

impl SearchService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
