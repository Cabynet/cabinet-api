use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::TaskStatusDto;
use super::service::TasksService;

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/import-html", post(import_html_handler))
        .route("/:id", get(get_task_status_handler))
        .route("/:id/retry", post(retry_import))
        .route("/export-html", get(export_bookmarks_handler))
        .layer(axum::extract::DefaultBodyLimit::max(60 * 1024 * 1024))
}

#[utoipa::path(
    get,
    path = "/v1/tasks/{id}",
    params(
        ("id" = Uuid, Path, description = "Task ID")
    ),
    responses(
        (status = 200, description = "Task status and progress", body = TaskStatusDto)
    ),
    tag = "tasks"
)]
pub async fn get_task_status_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<TaskStatusDto>, AppError> {
    let service = TasksService::new(pool, None);
    let task = service.get_task_status(user.id, id).await?;
    Ok(Json(task))
}

#[utoipa::path(
    post,
    path = "/v1/tasks/import-html",
    responses(
        (status = 202, description = "Durable import task accepted", body = TaskStatusDto)
    ),
    tag = "tasks"
)]
pub async fn import_html_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<TaskStatusDto>), AppError> {
    let service = TasksService::new(pool, crate::infra::redis::shared().await);

    let mut html_content = String::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" || name == "html" {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to read file field: {}", e)))?;
            html_content = String::from_utf8_lossy(&bytes).to_string();
            break;
        }
    }

    if html_content.trim().is_empty() {
        return Err(AppError::BadRequest("No HTML bookmark file provided".to_string()));
    }

    let task = service.import_html(user.id, &html_content).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/export",
    responses(
        (status = 200, description = "Netscape Bookmark HTML file export")
    ),
    tag = "bookmarks"
)]
pub async fn export_bookmarks_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, AppError> {
    let service = TasksService::new(pool, None);
    let html = service.export_html(user.id).await?;

    let filename = format!("cabinet_bookmarks_{}.html", Utc::now().format("%Y-%m-%d"));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename))
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?,
    );

    Ok((StatusCode::OK, headers, html))
}

async fn retry_import(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<TaskStatusDto>, AppError> {
    let mut tx = pool.begin().await?;

    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;

    let active: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM background_tasks
            WHERE user_id = $1 AND kind = 'IMPORT_HTML' AND status IN ('PENDING', 'RUNNING')
        )
        "#,
    )
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;

    if active {
        return Err(AppError::Conflict("An import is already active".into()));
    }

    let r = sqlx::query(
        r#"
        UPDATE background_tasks
        SET status = 'PENDING',
            error_message = NULL,
            finished_at = NULL,
            updated_at = now()
        WHERE id = $1
          AND user_id = $2
          AND kind = 'IMPORT_HTML'
          AND status = 'FAILED'
          AND payload IS NOT NULL
        "#,
    )
    .bind(id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;

    if r.rows_affected() != 1 {
        return Err(AppError::NotFound("Retryable import not found".into()));
    }

    tx.commit().await?;

    let service = TasksService::new(pool, None);
    let status = service.get_task_status(user.id, id).await?;
    service.start_task(id);

    Ok(Json(status))
}
