use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{CreateFolderRequest, FolderDto, UpdateFolderRequest};
use super::service::FoldersService;
use crate::modules::responses::{AcceptedInvitationResponse, CopiedShareResponse, ShareTokenResponse};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicFolderDto {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicBookmarkItem {
    pub id: Uuid,
    pub title: String,
    pub raw_url: Option<String>,
    pub normalized_domain: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicShareResponse {
    pub folder: PublicFolderDto,
    pub bookmarks: Vec<PublicBookmarkItem>,
    /// What the link grants; CONTRIBUTOR and EDITOR let a signed-in visitor join.
    pub role: String,
}

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/", get(list_folders).post(create_folder))
        .merge(super::collaboration::router())
        .route("/:id", put(update_folder).delete(delete_folder))
        .route("/:id/share", get(share_status).post(enable_share).put(enable_share).patch(update_share).delete(revoke_share))
}

pub fn public_share_router() -> Router<PgPool> {
    public_share_router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn public_share_router_routes() -> Router<PgPool> {
    Router::new()
        .route("/:token", get(get_public_share))
        .route("/:token/copy", post(copy_public_share))
        .route("/:token/join", post(join_public_share))
}

#[utoipa::path(
    get,
    path = "/v1/folders",
    responses(
        (status = 200, description = "List of user folders", body = Vec<FolderDto>)
    ),
    tag = "folders"
)]
pub async fn list_folders(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<FolderDto>>, AppError> {
    let service = FoldersService::new(pool);
    let folders = service.list_folders(user.id).await?;
    Ok(Json(folders))
}

#[utoipa::path(
    post,
    path = "/v1/folders",
    request_body = CreateFolderRequest,
    responses(
        (status = 201, description = "Folder created successfully", body = FolderDto)
    ),
    tag = "folders"
)]
pub async fn create_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<CreateFolderRequest>,
) -> Result<(StatusCode, Json<FolderDto>), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = FoldersService::new(pool);
    let folder = service.create_folder(user.id, payload).await?;
    Ok((StatusCode::CREATED, Json(folder)))
}

#[utoipa::path(
    put,
    path = "/v1/folders/{id}",
    params(
        ("id" = Uuid, Path, description = "Folder ID")
    ),
    request_body = UpdateFolderRequest,
    responses(
        (status = 200, description = "Updated folder", body = FolderDto)
    ),
    tag = "folders"
)]
pub async fn update_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
    Json(payload): Json<UpdateFolderRequest>,
) -> Result<Json<FolderDto>, AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let service = FoldersService::new(pool);
    let folder = service.update_folder(user.id, folder_id, payload).await?;
    Ok(Json(folder))
}

#[utoipa::path(
    delete,
    path = "/v1/folders/{id}",
    params(
        ("id" = Uuid, Path, description = "Folder ID")
    ),
    responses(
        (status = 204, description = "Folder deleted successfully")
    ),
    tag = "folders"
)]
pub async fn delete_folder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let service = FoldersService::new(pool);
    service.delete_folder(user.id, folder_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/v1/folders/{id}/share",
    params(
        ("id" = Uuid, Path, description = "Folder ID")
    ),
    responses(
        (status = 200, description = "Share token generated", body = ShareTokenResponse)
    ),
    tag = "folders"
)]
pub async fn enable_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
    body: Option<Json<ShareEnableRequest>>,
) -> Result<Json<crate::modules::responses::ShareTokenResponse>, AppError> {
    let role = body.and_then(|Json(req)| req.role).unwrap_or_else(|| "VIEWER".into());
    super::collaboration::validate_role(&role)?;
    let service = FoldersService::new(pool);
    let token = service.enable_share(user.id, folder_id, &role).await?;
    Ok(Json(ShareTokenResponse { share_token: token.into(), expires_at: None, role }))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShareEnableRequest {
    /// What the new link grants; defaults to VIEWER.
    pub role: Option<String>,
}

/// Change an active link's role, its expiry, or both. Absent fields stay unchanged.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShareUpdateRequest {
    pub role: Option<String>,
    /// Days from now until the link stops working; null removes the expiry.
    #[serde(default, deserialize_with = "super::dto::double_option")]
    #[schema(value_type = Option<u32>)]
    pub expires_in_days: Option<Option<u32>>,
}

#[utoipa::path(
    patch,
    path = "/v1/folders/{id}/share",
    params(
        ("id" = Uuid, Path, description = "Folder ID")
    ),
    request_body = ShareUpdateRequest,
    responses(
        (status = 200, description = "Link updated", body = ShareTokenResponse)
    ),
    tag = "folders"
)]
pub async fn update_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
    Json(req): Json<ShareUpdateRequest>,
) -> Result<Json<ShareTokenResponse>, AppError> {
    if req.role.is_none() && req.expires_in_days.is_none() {
        return Err(AppError::BadRequest("Nothing to update".into()));
    }
    if let Some(role) = &req.role {
        super::collaboration::validate_role(role)?;
    }
    let expires_at = req.expires_in_days.map(super::dto::link_expiry).transpose()?;
    let (token, role, expires_at) = FoldersService::new(pool)
        .update_share(user.id, folder_id, req.role.as_deref(), expires_at)
        .await?;
    Ok(Json(ShareTokenResponse { share_token: token, expires_at, role }))
}

#[utoipa::path(
    delete,
    path = "/v1/folders/{id}/share",
    params(
        ("id" = Uuid, Path, description = "Folder ID")
    ),
    responses(
        (status = 200, description = "Share revoked successfully", body = ShareTokenResponse)
    ),
    tag = "folders"
)]
pub async fn revoke_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::ShareTokenResponse>, AppError> {
    let service = FoldersService::new(pool);
    service.revoke_share(user.id, folder_id).await?;
    Ok(Json(ShareTokenResponse { share_token: None, expires_at: None, role: "VIEWER".into() }))
}

#[utoipa::path(
    get,
    path = "/v1/share/{token}",
    params(
        ("token" = String, Path, description = "Public share token")
    ),
    responses(
        (status = 200, description = "Read-only shared folder collection", body = PublicShareResponse),
        (status = 404, description = "Shared collection not found or expired")
    ),
    tag = "folders"
)]
pub async fn get_public_share(
    State(pool): State<PgPool>,
    Path(token): Path<String>,
) -> Result<Json<PublicShareResponse>, AppError> {
    let service = FoldersService::new(pool.clone());
    let (folder, owner_id, role) = service.get_shared_folder(&token).await?;

    // Fetch bookmarks for this folder, excluding notes, archived items, and deleted items
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            DateTime<Utc>,
        ),
    >(
        r#"
        SELECT id, title, raw_url, normalized_domain, description, site_name, thumbnail_url, favicon_url, created_at
        FROM bookmarks
        WHERE folder_id = $1
          AND user_id = $2
          AND deleted_at IS NULL
          AND is_archived = FALSE
          AND item_type IN ('LINK', 'FILE')
          AND raw_url IS NOT NULL
        ORDER BY is_pinned DESC, position ASC, created_at DESC
        "#,
    )
    .bind(folder.id)
    .bind(owner_id)
    .fetch_all(&pool)
    .await?;

    let bookmarks = rows
        .into_iter()
        .map(|r| PublicBookmarkItem {
            id: r.0,
            title: r.1,
            raw_url: r.2,
            normalized_domain: r.3,
            description: r.4,
            site_name: r.5,
            thumbnail_url: r.6,
            favicon_url: r.7,
            created_at: r.8,
        })
        .collect();

    Ok(Json(PublicShareResponse {
        folder: PublicFolderDto {
            id: folder.id,
            name: folder.name,
            description: folder.description,
        },
        bookmarks,
        role,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/share/{token}/join",
    params(
        ("token" = String, Path, description = "Public share token")
    ),
    responses(
        (status = 200, description = "Joined the folder with the link's role", body = AcceptedInvitationResponse),
        (status = 404, description = "Shared collection not found or expired")
    ),
    tag = "folders"
)]
/// Opens a shared folder in the visitor's own Cabinet, like Google Drive's "Shared with me":
/// the visitor becomes a member with the link's role. An existing higher role is kept.
pub async fn join_public_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(token): Path<String>,
) -> Result<Json<AcceptedInvitationResponse>, AppError> {
    use sha2::{Digest, Sha256};
    let mut tx = pool.begin().await?;
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();
    // Lock the folder row so turning the link off cannot race admission.
    let (folder_id, owner_id, role): (Uuid, Uuid, String) = sqlx::query_as(
        "SELECT id, user_id, share_role FROM folders WHERE share_token_hash = $1 AND (share_expires_at IS NULL OR share_expires_at > now()) FOR SHARE",
    )
    .bind(token_hash)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Shared collection not found".into()))?;

    if owner_id != user.id {
        sqlx::query(
            r#"
            INSERT INTO folder_members (folder_id, user_id, role) VALUES ($1, $2, $3)
            ON CONFLICT (folder_id, user_id) DO UPDATE SET role = CASE
                WHEN array_position(ARRAY['VIEWER', 'CONTRIBUTOR', 'EDITOR'], EXCLUDED.role)
                   > array_position(ARRAY['VIEWER', 'CONTRIBUTOR', 'EDITOR'], folder_members.role)
                THEN EXCLUDED.role ELSE folder_members.role END
            "#,
        )
        .bind(folder_id)
        .bind(user.id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Json(AcceptedInvitationResponse { folder_id }))
}

#[utoipa::path(
    post,
    path = "/v1/share/{token}/copy",
    params(
        ("token" = String, Path, description = "Share token"),
    ),
    responses(
        (status = 200, description = "OK", body = CopiedShareResponse)
    ),
    tag = "folders"
)]
pub async fn copy_public_share(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(token): Path<String>,
) -> Result<Json<crate::modules::responses::CopiedShareResponse>, AppError> {
    use sha2::{Digest, Sha256};
    let mut tx = pool.begin().await?;

    let token_hash = Sha256::digest(token.as_bytes()).to_vec();
    let (source, name): (Uuid, String) = sqlx::query_as(
        "SELECT id, name FROM folders WHERE share_token_hash = $1 AND (share_expires_at IS NULL OR share_expires_at > now()) FOR SHARE",
    )
    .bind(token_hash)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Shared collection not found".into()))?;

    let folder = Uuid::now_v7();
    sqlx::query("INSERT INTO folders (id, user_id, name) VALUES ($1, $2, $3)")
        .bind(folder)
        .bind(user.id)
        .bind(name)
        .execute(&mut *tx)
        .await?;

    let ids: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM bookmarks
        WHERE folder_id = $1
          AND user_id = (SELECT user_id FROM folders WHERE id = $1)
          AND deleted_at IS NULL
          AND is_archived = false
          AND item_type IN ('LINK', 'FILE')
          AND raw_url IS NOT NULL
        ORDER BY created_at, id
        "#,
    )
    .bind(source)
    .fetch_all(&mut *tx)
    .await?;

    for id in &ids {
        sqlx::query(
            r#"
            INSERT INTO bookmarks (
                id, user_id, folder_id, item_type, raw_url, normalized_url, url_hash,
                normalized_domain, title, description, site_name, thumbnail_url, favicon_url,
                content_type, metadata_status, user_edited_title
            )
            SELECT $1, $2, $3, item_type, raw_url, normalized_url, url_hash,
                   normalized_domain, title, description, site_name, thumbnail_url, favicon_url,
                   content_type, 'COMPLETED', true
            FROM bookmarks
            WHERE id = $4
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(user.id)
        .bind(folder)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Json(CopiedShareResponse { folder_id: folder, copied: ids.len() }))
}

#[utoipa::path(
    get,
    path = "/v1/folders/{id}/share",
    params(
        ("id" = Uuid, Path, description = "Folder ID"),
    ),
    responses(
        (status = 200, description = "OK", body = ShareTokenResponse)
    ),
    tag = "folders"
)]
pub async fn share_status(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::ShareTokenResponse>, AppError> {
    let (token, expires_at, role): (Option<String>, Option<DateTime<Utc>>, String) =
        sqlx::query_as("SELECT share_token, share_expires_at, share_role FROM folders WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user.id)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Folder not found".into()))?;

    // An expired link reads as turned off.
    if expires_at.is_some_and(|at| at <= Utc::now()) {
        return Ok(Json(ShareTokenResponse { share_token: None, expires_at: None, role: "VIEWER".into() }));
    }
    Ok(Json(ShareTokenResponse { share_token: token, expires_at, role }))
}
