use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNoteRequest {
    #[serde(alias = "base_revision")]
    pub base_revision: i32,
    #[serde(alias = "save_id")]
    pub save_id: Option<Uuid>,
    #[schema(value_type = Option<Object>)]
    pub document: Option<JsonValue>,
    #[serde(alias = "plain_text")]
    pub plain_text: Option<String>,
    pub title: Option<String>,
    #[serde(alias = "folder_id")]
    pub folder_id: Option<Uuid>,
    #[serde(alias = "set_folder")]
    pub set_folder: Option<bool>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoteResponse {
    pub id: Uuid,
    pub revision: i32,
    pub save_id: Option<Uuid>,
    #[schema(value_type = Option<Object>)]
    pub document: Option<JsonValue>,
    pub plain_text: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoteConflictError {
    pub code: String,
    pub message: String,
    pub current_revision: i32,
}
