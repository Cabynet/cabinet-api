use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use sqlx::PgPool;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{SearchQuery, SearchResponse};
use super::service::SearchService;

pub fn router() -> Router<PgPool> {
    Router::new().route("/", get(search_handler))
}

pub async fn search_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, AppError> {
    let service = SearchService::new(pool);
    let resp = service.search(user.id, query).await?;
    Ok(Json(resp))
}
