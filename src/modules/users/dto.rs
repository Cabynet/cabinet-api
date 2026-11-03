use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Validate, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUserRequest {
    #[validate(length(min = 2, max = 100, message = "Name must be between 2 and 100 characters"))]
    pub name: Option<String>,
    #[serde(alias = "avatar_url")]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Validate, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordRequest {
    #[serde(alias = "current_password")]
    pub current_password: Option<String>,
    #[validate(length(min = 8, message = "New password must be at least 8 characters"))]
    #[serde(alias = "new_password")]
    pub new_password: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserActionResponse {
    pub success: bool,
    pub message: String,
}

/// Password accounts must re-enter their password; OAuth-only accounts have none to check.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountRequest {
    #[serde(alias = "current_password")]
    pub current_password: Option<String>,
}
