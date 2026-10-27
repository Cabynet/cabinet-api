use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use super::dto::{CreateTagRequest, TagDto};

#[derive(Clone)]
pub struct TagsService {
    pub pool: PgPool,
}

pub fn slugify(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

impl TagsService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_tags(&self, user_id: Uuid) -> Result<Vec<TagDto>, AppError> {
        let rows = sqlx::query_as::<_, (Uuid, String, String, i64, DateTime<Utc>)>(
            r#"
            SELECT t.id, t.name, t.slug, COUNT(bt.bookmark_id)::bigint as bookmark_count, t.created_at
            FROM tags t
            LEFT JOIN bookmark_tags bt ON bt.tag_id = t.id
            WHERE t.user_id = $1
            GROUP BY t.id, t.name, t.slug, t.created_at
            ORDER BY t.name ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let tags = rows
            .into_iter()
            .map(|(id, name, slug, bookmark_count, created_at)| TagDto {
                id,
                name,
                slug,
                bookmark_count,
                created_at,
            })
            .collect();

        Ok(tags)
    }

    pub async fn create_tag(
        &self,
        user_id: Uuid,
        req: CreateTagRequest,
    ) -> Result<TagDto, AppError> {
        let name = req.name.trim().to_string();
        let slug = slugify(&name);

        if slug.is_empty() {
            return Err(AppError::BadRequest("Tag name must contain alphanumeric characters".to_string()));
        }

        let tag_id = Uuid::now_v7();

        let row = sqlx::query_as::<_, (Uuid, String, String, DateTime<Utc>)>(
            r#"
            INSERT INTO tags (id, user_id, name, slug, created_at)
            VALUES ($1, $2, $3, $4, NOW())
            ON CONFLICT (user_id, slug) DO UPDATE SET name = EXCLUDED.name
            RETURNING id, name, slug, created_at
            "#,
        )
        .bind(tag_id)
        .bind(user_id)
        .bind(&name)
        .bind(&slug)
        .fetch_one(&self.pool)
        .await?;

        Ok(TagDto {
            id: row.0,
            name: row.1,
            slug: row.2,
            bookmark_count: 0,
            created_at: row.3,
        })
    }

    pub async fn delete_tag(&self, user_id: Uuid, tag_id: Uuid) -> Result<(), AppError> {
        let res = sqlx::query(
            "DELETE FROM tags WHERE id = $1 AND user_id = $2",
        )
        .bind(tag_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            return Err(AppError::NotFound("Tag not found".to_string()));
        }

        Ok(())
    }
}
