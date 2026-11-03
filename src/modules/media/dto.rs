use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Validate, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PresignUploadRequest {
    #[validate(length(min = 1, max = 255, message = "Filename must be between 1 and 255 characters"))]
    pub filename: String,
    #[serde(alias = "mime_type")]
    pub mime_type: String,
    #[serde(alias = "size_bytes")]
    pub size_bytes: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PresignUploadResponse {
    pub asset_id: Uuid,
    pub upload_url: String,
    pub public_url: String,
    pub object_key: String,
    pub expires_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmUploadResponse {
    pub asset_id: Uuid,
    pub status: String,
    pub public_url: String,
}
