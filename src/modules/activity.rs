use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/", get(list))
        .route("/metadata/:id/retry", post(retry))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityTask {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: String,
    pub status: String,
    pub total_items: i32,
    pub processed_items: i32,
    pub failed_items: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub attempt: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityMetadata {
    pub id: Uuid,
    pub title: Option<String>,
    pub url: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityResponse {
    pub tasks: Vec<ActivityTask>,
    pub metadata: Vec<ActivityMetadata>,
    pub page: i64,
    pub has_more: bool,
}

#[derive(Deserialize)]
pub struct Page {
    page: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/v1/activity",
    params(
        ("page" = Option<i64>, Query, description = "1-based page"),
    ),
    responses(
        (status = 200, description = "OK", body = ActivityResponse)
    ),
    tag = "activity"
)]
pub async fn list(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<Page>,
) -> Result<Json<ActivityResponse>, AppError> {
    let page = q.page.unwrap_or(1).clamp(1, 10000);
    let offset = (page - 1) * 30;

    type TaskRow = (Uuid, Uuid, String, String, i32, i32, i32, Option<String>, DateTime<Utc>, Option<DateTime<Utc>>, i32, DateTime<Utc>);
    let mut tasks: Vec<TaskRow> = sqlx::query_as(
        r#"
        SELECT id, user_id, kind, status, total_items, processed_items, failed_items,
               error_message, created_at, finished_at, attempt, updated_at
        FROM background_tasks
        WHERE user_id = $1
        ORDER BY created_at DESC, id DESC
        LIMIT 31 OFFSET $2
        "#,
    )
    .bind(user.id)
    .bind(offset)
    .fetch_all(&pool)
    .await?;

    let mut metadata: Vec<(Uuid, Option<String>, Option<String>, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        r#"
        SELECT id, title, raw_url, metadata_status, metadata_error, updated_at
        FROM bookmarks
        WHERE user_id = $1
          AND item_type = 'LINK'
          AND deleted_at IS NULL
        ORDER BY
            CASE
                WHEN metadata_status = 'FAILED' THEN 0
                WHEN metadata_status IN ('PENDING', 'PROCESSING') THEN 1
                ELSE 2
            END,
            updated_at DESC,
            id DESC
        LIMIT 31 OFFSET $2
        "#,
    )
    .bind(user.id)
    .bind(offset)
    .fetch_all(&pool)
    .await?;

    let has_more = tasks.len() > 30 || metadata.len() > 30;
    tasks.truncate(30);
    metadata.truncate(30);

    Ok(Json(ActivityResponse {
        tasks: tasks
            .into_iter()
            .map(|t| ActivityTask {
                id: t.0, user_id: t.1, kind: t.2, status: t.3, total_items: t.4, processed_items: t.5,
                failed_items: t.6, error_message: t.7, created_at: t.8, finished_at: t.9, attempt: t.10, updated_at: t.11,
            })
            .collect(),
        metadata: metadata
            .into_iter()
            .map(|m| ActivityMetadata { id: m.0, title: m.1, url: m.2, status: m.3, error: m.4, updated_at: m.5 })
            .collect(),
        page,
        has_more,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/activity/metadata/{id}/retry",
    params(
        ("id" = Uuid, Path, description = "Resource ID"),
    ),
    responses(
        (status = 200, description = "OK", body = StatusResponse)
    ),
    tag = "activity"
)]
pub async fn retry(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::StatusResponse>, AppError> {
    let r = sqlx::query(
        r#"
        UPDATE bookmarks
        SET metadata_status = 'PENDING',
            metadata_attempts = 0,
            metadata_started_at = NULL,
            metadata_error = NULL,
            updated_at = now()
        WHERE id = $1
          AND user_id = $2
          AND item_type = 'LINK'
          AND metadata_status = 'FAILED'
          AND deleted_at IS NULL
        "#,
    )
    .bind(id)
    .bind(user.id)
    .execute(&pool)
    .await?;

    if r.rows_affected() != 1 {
        return Err(AppError::NotFound("Failed bookmark not found".into()));
    }

    crate::worker::queue::wake_workers();
    Ok(Json(crate::modules::responses::StatusResponse { status: "PENDING".into() }))
}
