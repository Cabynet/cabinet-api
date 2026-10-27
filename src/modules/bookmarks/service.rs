use sqlx::PgPool;

#[derive(Clone)]
pub struct BookmarksService {
    pub pool: PgPool,
}

impl BookmarksService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
