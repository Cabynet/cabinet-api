use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateNoteRequest {
    pub base_revision: i32,
    pub save_id: Option<Uuid>,
    pub document: Option<JsonValue>,
    pub plain_text: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NoteResponse {
    pub id: Uuid,
    pub revision: i32,
    pub save_id: Option<Uuid>,
    pub document: Option<JsonValue>,
    pub plain_text: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NoteConflictError {
    pub code: String,
    pub message: String,
    pub current_revision: i32,
}
