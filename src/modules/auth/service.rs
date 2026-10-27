use sqlx::PgPool;

#[derive(Clone)]
pub struct AuthService {
    pub pool: PgPool,
}

impl AuthService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
