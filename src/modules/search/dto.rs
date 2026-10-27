use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SearchQuery {
    pub q: String,
    pub folder_id: Option<Uuid>,
    pub tag: Option<String>,
    pub domain: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchResultItem {
    pub id: Uuid,
    pub title: String,
    pub raw_url: Option<String>,
    pub description: Option<String>,
    pub rank: f32,
}
