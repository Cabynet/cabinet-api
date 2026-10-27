use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const QUEUE_INTERACTIVE: &str = "cabinet:queue:interactive";
pub const QUEUE_BULK: &str = "cabinet:queue:bulk";

#[derive(Debug, Serialize, Deserialize)]
pub struct EnrichmentJob {
    pub bookmark_id: Uuid,
    pub user_id: Uuid,
    pub url: String,
    pub url_hash: Vec<u8>,
    pub attempt: u32,
}
