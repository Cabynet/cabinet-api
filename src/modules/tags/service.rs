use sqlx::PgPool;

#[derive(Clone)]
pub struct TagsService {
    pub pool: PgPool,
}

impl TagsService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
