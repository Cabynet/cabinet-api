use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Validate, ToSchema)]
pub struct PresignUploadRequest {
    #[validate(length(min = 1, max = 255, message = "Filename must be between 1 and 255 characters"))]
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PresignUploadResponse {
    pub asset_id: Uuid,
    pub upload_url: String,
    pub public_url: String,
    pub object_key: String,
    pub expires_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ConfirmUploadResponse {
    pub asset_id: Uuid,
    pub status: String,
    pub public_url: String,
}
