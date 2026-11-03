use axum::{
    extract::{Path, State},
    routing::{get, patch, post, put},
    Json, Router,
};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::responses::{AcceptedInvitationResponse, CollaborationResponse, FolderMemberDto, FolderUserDto, InvitationTokenResponse, MemberAddedResponse, RemovedResponse, UpdatedResponse};
use crate::modules::auth::middleware::CurrentUser;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/:id/collaboration", get(collaboration))
        .route("/:id/members", post(add_member))
        .route("/:id/members/:member", patch(change_member).delete(remove_member))
        .route("/:id/invitation", put(invite).patch(change_invite).delete(remove_invite))
        .route("/invitations/:token/accept", post(accept))
}

pub async fn access(pool: &PgPool, actor: Uuid, folder: Uuid) -> Result<String, AppError> {
    let role: Option<String> = sqlx::query_scalar(
        r#"
        SELECT CASE
            WHEN f.user_id = $2 THEN 'OWNER'
            ELSE m.role
        END
        FROM folders f
        LEFT JOIN folder_members m ON m.folder_id = f.id AND m.user_id = $2
        WHERE f.id = $1 AND (f.user_id = $2 OR m.user_id IS NOT NULL)
        "#,
    )
    .bind(folder)
    .bind(actor)
    .fetch_optional(pool)
    .await?;

    role.ok_or_else(|| AppError::NotFound("Folder not found".into()))
}

async fn require_owner(pool: &PgPool, actor: Uuid, folder: Uuid) -> Result<(), AppError> {
    if access(pool, actor, folder).await? != "OWNER" {
        return Err(AppError::Forbidden(
            "Only the folder owner may manage access".into(),
        ));
    }
    Ok(())
}

fn validate_role(role: &str) -> Result<(), AppError> {
    if !["VIEWER", "CONTRIBUTOR", "EDITOR"].contains(&role) {
        return Err(AppError::BadRequest("Invalid folder role".into()));
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/v1/folders/{id}/collaboration",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
    ),
    responses(
        (status = 200, description = "OK", body = CollaborationResponse)
    ),
    tag = "folders"
)]
pub async fn collaboration(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::CollaborationResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;

    let owner: (Uuid, String, String) = sqlx::query_as(
        r#"
        SELECT u.id, u.name, u.email
        FROM folders f
        JOIN users u ON u.id = f.user_id
        WHERE f.id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;

    let members: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        r#"
        SELECT u.id, m.role, u.name, u.email
        FROM folder_members m
        JOIN users u ON u.id = m.user_id
        WHERE m.folder_id = $1
        ORDER BY m.created_at, u.id
        "#,
    )
    .bind(id)
    .fetch_all(&pool)
    .await?;

    let invitation: Option<String> = sqlx::query_scalar(
        "SELECT role FROM folder_invitations WHERE folder_id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await?;

    Ok(Json(CollaborationResponse {
        owner: FolderUserDto { id: owner.0, name: owner.1, email: owner.2 },
        members: members
            .into_iter()
            .map(|(id, role, name, email)| FolderMemberDto {
                user_id: id,
                role,
                user: FolderUserDto { id, name, email },
            })
            .collect(),
        invite_enabled: invitation.is_some(),
        invite_role: invitation,
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct MemberRequest {
    email: String,
    role: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RoleRequest {
    role: String,
}

#[utoipa::path(
    post,
    path = "/v1/folders/{id}/members",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
    ),
    responses(
        (status = 200, description = "OK", body = MemberAddedResponse)
    ),
    tag = "folders"
)]
pub async fn add_member(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MemberRequest>,
) -> Result<Json<crate::modules::responses::MemberAddedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    validate_role(&req.role)?;

    let member: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = lower($1)")
        .bind(req.email.trim())
        .fetch_optional(&pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Ask this person to create an account first".into()))?;

    if member == user.id {
        return Err(AppError::BadRequest("The owner already has access".into()));
    }

    sqlx::query(
        r#"
        INSERT INTO folder_members (folder_id, user_id, role)
        VALUES ($1, $2, $3)
        ON CONFLICT (folder_id, user_id) DO UPDATE SET role = $3
        "#,
    )
    .bind(id)
    .bind(member)
    .bind(req.role)
    .execute(&pool)
    .await?;

    Ok(Json(MemberAddedResponse { user_id: member }))
}

#[utoipa::path(
    patch,
    path = "/v1/folders/{id}/members/{member}",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
        ("member" = Uuid, Path, description = "Member user ID"),
    ),
    responses(
        (status = 200, description = "OK", body = UpdatedResponse)
    ),
    tag = "folders"
)]
pub async fn change_member(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path((id, member)): Path<(Uuid, Uuid)>,
    Json(req): Json<RoleRequest>,
) -> Result<Json<crate::modules::responses::UpdatedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    validate_role(&req.role)?;

    let r = sqlx::query("UPDATE folder_members SET role = $3 WHERE folder_id = $1 AND user_id = $2")
        .bind(id)
        .bind(member)
        .bind(req.role)
        .execute(&pool)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Member not found".into()));
    }

    Ok(Json(UpdatedResponse { updated: true }))
}

#[utoipa::path(
    delete,
    path = "/v1/folders/{id}/members/{member}",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
        ("member" = Uuid, Path, description = "Member user ID"),
    ),
    responses(
        (status = 200, description = "OK", body = RemovedResponse)
    ),
    tag = "folders"
)]
pub async fn remove_member(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path((id, member)): Path<(Uuid, Uuid)>,
) -> Result<Json<crate::modules::responses::RemovedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM folder_members WHERE folder_id = $1 AND user_id = $2")
        .bind(id)
        .bind(member)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM folder_invitations WHERE folder_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(Json(RemovedResponse { removed: true }))
}

#[utoipa::path(
    put,
    path = "/v1/folders/{id}/invitation",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
    ),
    responses(
        (status = 200, description = "OK", body = InvitationTokenResponse)
    ),
    tag = "folders"
)]
pub async fn invite(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<RoleRequest>,
) -> Result<Json<crate::modules::responses::InvitationTokenResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    validate_role(&req.role)?;

    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();

    sqlx::query(
        r#"
        INSERT INTO folder_invitations (folder_id, token_hash, role)
        VALUES ($1, $2, $3)
        ON CONFLICT (folder_id) DO UPDATE SET token_hash = $2, role = $3, created_at = now()
        "#,
    )
    .bind(id)
    .bind(token_hash)
    .bind(req.role)
    .execute(&pool)
    .await?;

    Ok(Json(InvitationTokenResponse { token }))
}

#[utoipa::path(
    patch,
    path = "/v1/folders/{id}/invitation",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
    ),
    responses(
        (status = 200, description = "OK", body = UpdatedResponse)
    ),
    tag = "folders"
)]
pub async fn change_invite(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<RoleRequest>,
) -> Result<Json<crate::modules::responses::UpdatedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    validate_role(&req.role)?;

    let r = sqlx::query("UPDATE folder_invitations SET role = $2 WHERE folder_id = $1")
        .bind(id)
        .bind(req.role)
        .execute(&pool)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Invitation not found".into()));
    }

    Ok(Json(UpdatedResponse { updated: true }))
}

#[utoipa::path(
    delete,
    path = "/v1/folders/{id}/invitation",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
    ),
    responses(
        (status = 200, description = "OK", body = RemovedResponse)
    ),
    tag = "folders"
)]
pub async fn remove_invite(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::RemovedResponse>, AppError> {
    require_owner(&pool, user.id, id).await?;
    sqlx::query("DELETE FROM folder_invitations WHERE folder_id = $1")
        .bind(id)
        .execute(&pool)
        .await?;
    Ok(Json(RemovedResponse { removed: true }))
}

#[utoipa::path(
    post,
    path = "/v1/folders/invitations/{token}/accept",
    params(
        ("token" = String, Path, description = "Invitation token"),
    ),
    responses(
        (status = 200, description = "OK", body = AcceptedInvitationResponse)
    ),
    tag = "folders"
)]
pub async fn accept(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(token): Path<String>,
) -> Result<Json<crate::modules::responses::AcceptedInvitationResponse>, AppError> {
    let mut tx = pool.begin().await?;
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();

    // Lock the invitation so revocation/role changes cannot race admission.
    let (id, role): (Uuid, String) = sqlx::query_as(
        "SELECT folder_id, role FROM folder_invitations WHERE token_hash = $1 FOR UPDATE",
    )
    .bind(token_hash)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Invitation expired or revoked".into()))?;

    sqlx::query(
        "INSERT INTO folder_members (folder_id, user_id, role) VALUES ($1, $2, $3) ON CONFLICT (folder_id, user_id) DO NOTHING",
    )
    .bind(id)
    .bind(user.id)
    .bind(role)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(AcceptedInvitationResponse { folder_id: id }))
}
