use sqlx::PgPool;

#[derive(Clone)]
pub struct NotesService {
    pub pool: PgPool,
}

impl NotesService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
