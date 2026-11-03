use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusDto {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub total_items: i32,
    pub processed_items: i32,
    pub failed_items: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}
