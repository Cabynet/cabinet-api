use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FolderDto {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub access: String,
    pub position: i32,
    pub share_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize, Validate, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateFolderRequest {
    #[validate(length(min = 1, max = 120, message = "Folder name must be between 1 and 120 characters"))]
    pub name: String,
    #[validate(length(max = 500, message = "Description must not exceed 500 characters"))]
    pub description: Option<String>,
    #[serde(alias = "parent_id")]
    pub parent_id: Option<Uuid>,
}

/// Longest a share or invite link may stay valid.
pub const MAX_LINK_EXPIRY_DAYS: u32 = 365;

/// Turns "expires in N days" into a timestamp; None never expires.
pub fn link_expiry(days: Option<u32>) -> Result<Option<chrono::DateTime<chrono::Utc>>, crate::errors::AppError> {
    match days {
        None => Ok(None),
        Some(d) if (1..=MAX_LINK_EXPIRY_DAYS).contains(&d) => Ok(Some(chrono::Utc::now() + chrono::Duration::days(d.into()))),
        Some(_) => Err(crate::errors::AppError::BadRequest(format!("expiresInDays must be between 1 and {MAX_LINK_EXPIRY_DAYS}"))),
    }
}

/// Distinguishes an absent field (None: unchanged) from an explicit null (Some(None): clear).
pub(crate) fn double_option<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Default, Serialize, Deserialize, Validate, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFolderRequest {
    #[validate(length(min = 1, max = 120, message = "Folder name must be between 1 and 120 characters"))]
    pub name: Option<String>,
    /// Absent: unchanged. null or empty: clear. Max 500 characters (checked in the service).
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    /// Absent: unchanged. null: move to root. uuid: new parent.
    #[serde(default, alias = "parent_id", deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Uuid>)]
    pub parent_id: Option<Option<Uuid>>,
    pub position: Option<i32>,
}

