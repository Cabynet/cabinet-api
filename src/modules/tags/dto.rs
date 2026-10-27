use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct TagDto {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub bookmark_count: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Validate, ToSchema)]
pub struct CreateTagRequest {
    #[validate(length(min = 1, max = 64, message = "Name must be between 1 and 64 characters"))]
    pub name: String,
}
