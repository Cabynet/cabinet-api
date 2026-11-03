//! Small typed response bodies shared by handlers that used to build them with `json!`.
//! Field names serialize as camelCase, identical to the previous wire format.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AffectedResponse {
    pub affected: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdatedResponse {
    pub updated: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RemovedResponse {
    pub removed: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MergedResponse {
    pub merged: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeletedCountResponse {
    pub deleted: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IdResponse {
    pub id: Uuid,
}

/// A queued background task: `status` is always `PENDING` at creation.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TaskQueuedResponse {
    pub id: Uuid,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatusResponse {
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShareTokenResponse {
    pub share_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CopiedShareResponse {
    pub folder_id: Uuid,
    pub copied: usize,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EmbeddableResponse {
    pub embeddable: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeResponse {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub favicon_url: Option<String>,
    pub site_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkHealthSummary {
    pub total: i64,
    pub broken: i64,
    pub ok: i64,
    pub healthy: i64,
    pub redirected: i64,
    pub favorites: i64,
    pub archived: i64,
    pub trash: i64,
    pub last_check: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceHealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OperationsAccessResponse {
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FolderUserDto {
    pub id: Uuid,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FolderMemberDto {
    pub user_id: Uuid,
    pub role: String,
    pub user: FolderUserDto,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationResponse {
    pub owner: FolderUserDto,
    pub members: Vec<FolderMemberDto>,
    pub invite_enabled: bool,
    pub invite_role: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberAddedResponse {
    pub user_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InvitationTokenResponse {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedInvitationResponse {
    pub folder_id: Uuid,
}
