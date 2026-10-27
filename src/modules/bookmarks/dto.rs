use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Validate, Clone, ToSchema)]
pub struct CreateBookmarkRequest {
    pub url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub folder_id: Option<Uuid>,
    #[serde(rename = "folderId")]
    pub folder_id_camel: Option<Uuid>,
    pub item_type: Option<String>,
    #[serde(rename = "type")]
    pub item_type_alias: Option<String>,
    pub client_request_id: Option<Uuid>,
    #[serde(rename = "clientRequestId")]
    pub client_request_id_camel: Option<Uuid>,
    pub tags: Option<Vec<String>>,
    pub data: Option<Box<CreateBookmarkRequest>>,
}

impl CreateBookmarkRequest {
    pub fn resolved(self) -> Self {
        if let Some(inner) = self.data {
            return inner.resolved();
        }
        let folder_id = self.folder_id.or(self.folder_id_camel);
        let item_type = self.item_type.or(self.item_type_alias);
        let client_request_id = self.client_request_id.or(self.client_request_id_camel);
        Self {
            url: self.url,
            title: self.title,
            description: self.description,
            folder_id,
            folder_id_camel: folder_id,
            item_type: item_type.clone(),
            item_type_alias: item_type,
            client_request_id,
            client_request_id_camel: client_request_id,
            tags: self.tags,
            data: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, ToSchema)]
pub struct UpdateBookmarkRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub folder_id: Option<Uuid>,
    #[serde(rename = "folderId")]
    pub folder_id_camel: Option<Uuid>,
    pub is_pinned: Option<bool>,
    #[serde(rename = "isPinned")]
    pub is_pinned_camel: Option<bool>,
    pub is_favorite: Option<bool>,
    #[serde(rename = "isFavorite")]
    pub is_favorite_camel: Option<bool>,
    pub is_archived: Option<bool>,
    #[serde(rename = "isArchived")]
    pub is_archived_camel: Option<bool>,
    pub tags: Option<Vec<String>>,
    pub data: Option<Box<UpdateBookmarkRequest>>,
}

impl UpdateBookmarkRequest {
    pub fn resolved(self) -> Self {
        if let Some(inner) = self.data {
            return inner.resolved();
        }
        let folder_id = self.folder_id.or(self.folder_id_camel);
        let is_pinned = self.is_pinned.or(self.is_pinned_camel);
        let is_favorite = self.is_favorite.or(self.is_favorite_camel);
        let is_archived = self.is_archived.or(self.is_archived_camel);
        Self {
            title: self.title,
            description: self.description,
            folder_id,
            folder_id_camel: folder_id,
            is_pinned,
            is_pinned_camel: is_pinned,
            is_favorite,
            is_favorite_camel: is_favorite,
            is_archived,
            is_archived_camel: is_archived,
            tags: self.tags,
            data: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct BookmarkTagSummary {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct BookmarkListItem {
    pub id: Uuid,
    pub item_type: String,
    pub raw_url: Option<String>,
    pub normalized_domain: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub is_pinned: bool,
    pub is_favorite: bool,
    pub is_archived: bool,
    pub folder_id: Option<Uuid>,
    pub metadata_status: String,
    pub tags: Vec<BookmarkTagSummary>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct ListBookmarksQuery {
    pub folder_id: Option<Uuid>,
    pub tag: Option<String>,
    pub domain: Option<String>,
    pub is_archived: Option<bool>,
    pub is_favorite: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BookmarkListResponse {
    pub items: Vec<BookmarkListItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BookmarkCursor {
    pub is_pinned: bool,
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

impl BookmarkCursor {
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, json)
    }

    pub fn decode(encoded: &str) -> Option<Self> {
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, encoded).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct BookmarkPreviewDto {
    pub id: Uuid,
    pub metadata_status: String,
    pub title: String,
    pub description: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub site_name: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct BookmarkPreviewsQuery {
    pub ids: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BookmarkPreviewsResponse {
    pub previews: Vec<BookmarkPreviewDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DuplicateItemDto {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DuplicateGroupDto {
    pub url: String,
    pub domain: Option<String>,
    pub count: usize,
    pub items: Vec<DuplicateItemDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DuplicateReportResponse {
    pub total_duplicate_groups: usize,
    pub total_redundant_items: usize,
    pub groups: Vec<DuplicateGroupDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DuplicateCleanupResponse {
    pub cleaned_items: usize,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LinkHealthCheckResponse {
    pub id: Uuid,
    pub url: Option<String>,
    pub link_health: String,
    pub http_status_code: Option<i16>,
}
