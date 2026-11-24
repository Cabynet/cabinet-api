//! Google Drive-style access flow: people without access request it, owners approve or deny,
//! and members can leave a folder shared with them.
use axum::{
    extract::{Path, State},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::collaboration::{access, validate_role};
use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use crate::modules::responses::{RemovedResponse, UpdatedResponse};

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/:id/access-requests", get(list_requests).post(request_access))
        .route("/:id/access-requests/mine", get(my_request))
        .route("/:id/access-requests/:user", delete(deny_request))
        .route("/:id/access-requests/:user/approve", post(approve_request))
        .route("/:id/membership", delete(leave_folder))
}

/// The actor's access to a folder, or None when they have none (unlike `access`, which 404s).
async fn current_access(pool: &PgPool, actor: Uuid, folder: Uuid) -> Result<Option<String>, AppError> {
    match access(pool, actor, folder).await {
        Ok(role) => Ok(Some(role)),
        Err(AppError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn require_owner(pool: &PgPool, actor: Uuid, folder: Uuid) -> Result<(), AppError> {
    if current_access(pool, actor, folder).await?.as_deref() != Some("OWNER") {
        return Err(AppError::Forbidden("Only the folder owner may manage access".into()));
    }
    Ok(())
}

#[derive(Deserialize, ToSchema)]
pub struct AccessRequestBody {
    /// An optional note for the owner, up to 500 characters.
    pub message: Option<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessRequestStatus {
    pub requested: bool,
}

#[utoipa::path(
    post,
    path = "/v1/folders/{id}/access-requests",
    params(("id" = Uuid, Path, description = "Folder ID")),
    request_body = AccessRequestBody,
    responses((status = 200, description = "Request recorded", body = AccessRequestStatus)),
    tag = "folders"
)]
pub async fn request_access(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AccessRequestBody>,
) -> Result<Json<AccessRequestStatus>, AppError> {
    let message = body.message.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty());
    if message.as_ref().is_some_and(|m| m.chars().count() > 500) {
        return Err(AppError::BadRequest("Keep the message under 500 characters".into()));
    }
    if current_access(&pool, user.id, id).await?.is_some() {
        return Err(AppError::Conflict("You already have access to this folder".into()));
    }
    // Asking again only refreshes the note, so the owner sees one request per person.
    let inserted = sqlx::query(
        r#"
        INSERT INTO folder_access_requests (folder_id, user_id, message)
        SELECT id, $2, $3 FROM folders WHERE id = $1
        ON CONFLICT (folder_id, user_id) DO UPDATE SET message = EXCLUDED.message, created_at = NOW()
        "#,
    )
    .bind(id)
    .bind(user.id)
    .bind(message)
    .execute(&pool)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::NotFound("Folder not found".into()));
    }
    Ok(Json(AccessRequestStatus { requested: true }))
}

#[utoipa::path(
    get,
    path = "/v1/folders/{id}/access-requests/mine",
    params(("id" = Uuid, Path, description = "Folder ID")),
    responses((status = 200, description = "Whether the caller has a pending request", body = AccessRequestStatus)),
    tag = "folders"
)]
pub async fn my_request(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AccessRequestStatus>, AppError> {
    let requested: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM folder_access_requests WHERE folder_id = $1 AND user_id = $2)")
        .bind(id)
        .bind(user.id)
        .fetch_one(&pool)
        .await?;
    Ok(Json(AccessRequestStatus { requested }))
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessRequestDto {
    pub user_id: Uuid,
    pub name: String,
    pub email: String,
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[utoipa::path(
    get,
    path = "/v1/folders/{id}/access-requests",
    params(("id" = Uuid, Path, description = "Folder ID")),
    responses((status = 200, description = "Pending requests, oldest first", body = [AccessRequestDto])),
    tag = "folders"
)]
pub async fn list_requests(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AccessRequestDto>>, AppError> {
    require_owner(&pool, user.id, id).await?;
    // People who got access another way (email, link) no longer need their request.
    let rows: Vec<(Uuid, String, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        r#"
        SELECT u.id, u.name, u.email, r.message, r.created_at
        FROM folder_access_requests r JOIN users u ON u.id = r.user_id
        WHERE r.folder_id = $1
          AND NOT EXISTS (SELECT 1 FROM folder_members m WHERE m.folder_id = r.folder_id AND m.user_id = r.user_id)
        ORDER BY r.created_at, u.id
        "#,
    )
    .bind(id)
    .fetch_all(&pool)
    .await?;
    Ok(Json(rows.into_iter().map(|(user_id, name, email, message, created_at)| AccessRequestDto { user_id, name, email, message, created_at }).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct ApproveRequestBody {
    pub role: String,
}

#[utoipa::path(
    post,
    path = "/v1/folders/{id}/access-requests/{user}/approve",
    params(("id" = Uuid, Path, description = "Folder ID"), ("user" = Uuid, Path, description = "Requesting user")),
    request_body = ApproveRequestBody,
    responses((status = 200, description = "The person now has access", body = UpdatedResponse)),
    tag = "folders"
)]
pub async fn approve_request(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path((id, requester)): Path<(Uuid, Uuid)>,
    Json(body): Json<ApproveRequestBody>,
) -> Result<Json<UpdatedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    validate_role(&body.role)?;
    let mut tx = pool.begin().await?;
    let removed = sqlx::query("DELETE FROM folder_access_requests WHERE folder_id = $1 AND user_id = $2")
        .bind(id)
        .bind(requester)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::NotFound("Access request not found".into()));
    }
    sqlx::query(
        "INSERT INTO folder_members (folder_id, user_id, role) VALUES ($1, $2, $3) ON CONFLICT (folder_id, user_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(id)
    .bind(requester)
    .bind(&body.role)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(UpdatedResponse { updated: true }))
}

#[utoipa::path(
    delete,
    path = "/v1/folders/{id}/access-requests/{user}",
    params(("id" = Uuid, Path, description = "Folder ID"), ("user" = Uuid, Path, description = "Requesting user")),
    responses((status = 200, description = "Request declined", body = RemovedResponse)),
    tag = "folders"
)]
pub async fn deny_request(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path((id, requester)): Path<(Uuid, Uuid)>,
) -> Result<Json<RemovedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    sqlx::query("DELETE FROM folder_access_requests WHERE folder_id = $1 AND user_id = $2")
        .bind(id)
        .bind(requester)
        .execute(&pool)
        .await?;
    Ok(Json(RemovedResponse { removed: true }))
}

#[utoipa::path(
    delete,
    path = "/v1/folders/{id}/membership",
    params(("id" = Uuid, Path, description = "Folder ID")),
    responses((status = 200, description = "Left the shared folder", body = RemovedResponse)),
    tag = "folders"
)]
/// A member leaves a folder shared with them; the owner can't leave their own folder.
pub async fn leave_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RemovedResponse>, AppError> {
    match current_access(&pool, user.id, id).await?.as_deref() {
        None => Err(AppError::NotFound("Folder not found".into())),
        Some("OWNER") => Err(AppError::BadRequest("You own this folder; delete it instead of leaving".into())),
        Some(_) => {
            sqlx::query("DELETE FROM folder_members WHERE folder_id = $1 AND user_id = $2")
                .bind(id)
                .bind(user.id)
                .execute(&pool)
                .await?;
            Ok(Json(RemovedResponse { removed: true }))
        }
    }
}
