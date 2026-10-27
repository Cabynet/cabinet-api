use axum::{routing::post, Router};
use sqlx::PgPool;

pub fn router() -> Router<PgPool> {
    Router::new().route("/register", post(|| async { "register" }))
}
