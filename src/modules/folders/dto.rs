use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct FolderDto {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub position: i32,
    pub share_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, ToSchema)]
pub struct CreateFolderRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    #[serde(rename = "parentId")]
    pub parent_id_camel: Option<Uuid>,
    pub data: Option<Box<CreateFolderRequest>>,
}

impl CreateFolderRequest {
    pub fn resolved(self) -> Self {
        if let Some(inner) = self.data {
            return inner.resolved();
        }
        let parent_id = self.parent_id.or(self.parent_id_camel);
        Self {
            name: self.name,
            description: self.description,
            parent_id,
            parent_id_camel: parent_id,
            data: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, ToSchema)]
pub struct UpdateFolderRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    #[serde(rename = "parentId")]
    pub parent_id_camel: Option<Uuid>,
    pub position: Option<i32>,
    pub data: Option<Box<UpdateFolderRequest>>,
}

impl UpdateFolderRequest {
    pub fn resolved(self) -> Self {
        if let Some(inner) = self.data {
            return inner.resolved();
        }
        let parent_id = self.parent_id.or(self.parent_id_camel);
        Self {
            name: self.name,
            description: self.description,
            parent_id,
            parent_id_camel: parent_id,
            position: self.position,
            data: None,
        }
    }
}
