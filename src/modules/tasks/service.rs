use sqlx::PgPool;

#[derive(Clone)]
pub struct TasksService {
    pub pool: PgPool,
}

impl TasksService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
