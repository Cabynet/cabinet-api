use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

// camelCase is canonical; the snake_case aliases keep older clients working.
#[derive(Debug, Default, Serialize, Deserialize, Validate, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateBookmarkRequest {
    pub url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(alias = "folder_id")]
    pub folder_id: Option<Uuid>,
    #[serde(alias = "type", alias = "item_type")]
    pub item_type: Option<String>,
    #[serde(alias = "client_request_id")]
    pub client_request_id: Option<Uuid>,
    pub tags: Option<Vec<String>>,
    #[serde(alias = "note_document")]
    pub note_document: Option<serde_json::Value>,
    #[serde(alias = "notes", alias = "notes_plain")]
    pub notes_plain: Option<String>,
    pub drawing: Option<serde_json::Value>,
    #[serde(alias = "imageUrl", alias = "thumbnail_url")]
    pub thumbnail_url: Option<String>,
    #[serde(alias = "favicon_url")]
    pub favicon_url: Option<String>,
    #[serde(alias = "content_type")]
    pub content_type: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, Validate, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBookmarkRequest {
    pub url: Option<String>,
    #[serde(alias = "notes_plain")]
    pub notes_plain: Option<String>,
    #[serde(alias = "base_revision")]
    pub base_revision: Option<i32>,
    #[serde(alias = "save_id")]
    pub save_id: Option<Uuid>,
    #[serde(alias = "set_folder")]
    pub set_folder: Option<bool>,
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(alias = "folder_id")]
    pub folder_id: Option<Uuid>,
    #[serde(alias = "is_pinned")]
    pub is_pinned: Option<bool>,
    #[serde(alias = "is_favorite")]
    pub is_favorite: Option<bool>,
    #[serde(alias = "is_archived")]
    pub is_archived: Option<bool>,
    pub tags: Option<Vec<String>>,
    #[serde(alias = "imageUrl", alias = "thumbnail_url")]
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkTagSummary {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkListItem {
    pub id: Uuid,
    pub user_id: Uuid,
    pub access: String,
    pub item_type: String,
    pub raw_url: Option<String>,
    pub normalized_domain: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub content_type: Option<String>,
    pub is_pinned: bool,
    pub is_favorite: bool,
    pub is_archived: bool,
    pub folder_id: Option<Uuid>,
    pub metadata_status: String,
    pub metadata_error: Option<String>,
    pub notes_plain: Option<String>,
    pub note_document: Option<serde_json::Value>,
    pub note_revision: i32,
    pub drawing: Option<serde_json::Value>,
    pub position: i32,
    pub tags: Vec<BookmarkTagSummary>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListBookmarksQuery {
    #[serde(alias = "folder_id")]
    pub folder_id: Option<Uuid>,
    pub tag: Option<String>,
    pub domain: Option<String>,
    #[serde(alias = "is_archived")]
    pub is_archived: Option<bool>,
    #[serde(alias = "is_favorite")]
    pub is_favorite: Option<bool>,
    pub trash: Option<bool>,
    pub status: Option<String>,
    pub search: Option<String>,
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    #[serde(alias = "content_type")]
    pub content_type: Option<String>,
    #[serde(alias = "added_after")]
    pub added_after: Option<DateTime<Utc>>,
    #[serde(alias = "added_before")]
    pub added_before: Option<DateTime<Utc>>,
    pub sort: Option<String>,
    pub summary: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkListResponse {
    pub items: Vec<BookmarkListItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub total_count: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkPreviewDto {
    pub id: Uuid,
    pub raw_url: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub metadata_status: String,
    pub title: String,
    pub description: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub site_name: Option<String>,
}
#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct BookmarkPreviewsQuery { pub ids: String }
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BookmarkPreviewsResponse { pub previews: Vec<BookmarkPreviewDto> }
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroupDto { pub url: String, pub domain: Option<String>, pub count: usize, pub items: Vec<BookmarkListItem> }
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateReportResponse { pub total_duplicate_groups: usize, pub total_redundant_items: usize, pub groups: Vec<DuplicateGroupDto> }
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateCleanupResponse { pub cleaned_items: usize, pub message: String }
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkHealthCheckResponse { pub id: Uuid, pub url: Option<String>, pub link_health: String, pub http_status_code: Option<i16> }
