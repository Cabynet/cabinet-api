use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::modules::bookmarks::dto::BookmarkTagSummary;

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct SearchQuery {
    pub q: String,
    pub folder_id: Option<Uuid>,
    pub tag: Option<String>,
    pub domain: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SearchResultItem {
    pub id: Uuid,
    pub title: String,
    pub raw_url: Option<String>,
    pub normalized_domain: Option<String>,
    pub description: Option<String>,
    pub thumbnail_url: Option<String>,
    pub favicon_url: Option<String>,
    pub score: f32,
    pub tags: Vec<BookmarkTagSummary>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SearchResponse {
    pub query: String,
    pub total: usize,
    pub items: Vec<SearchResultItem>,
}
