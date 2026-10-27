use sqlx::PgPool;

#[derive(Clone)]
pub struct FoldersService {
    pub pool: PgPool,
}

impl FoldersService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
