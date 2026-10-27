use chrono::{DateTime, Utc};
use redis::AsyncCommands;
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

use crate::errors::AppError;
use crate::worker::queue::{EnrichmentJob, QUEUE_INTERACTIVE};
use super::dto::{
    BookmarkCursor, BookmarkListItem, BookmarkListResponse, BookmarkTagSummary,
    CreateBookmarkRequest, ListBookmarksQuery, UpdateBookmarkRequest,
};
use super::normalizer::normalize_url;
use crate::modules::tags::service::slugify;

#[derive(Clone)]
pub struct BookmarksService {
    pub pool: PgPool,
    pub redis: Option<redis::aio::ConnectionManager>,
}

impl BookmarksService {
    pub fn new(pool: PgPool, redis: Option<redis::aio::ConnectionManager>) -> Self {
        Self { pool, redis }
    }

    pub async fn create_bookmark(
        &self,
        user_id: Uuid,
        req: CreateBookmarkRequest,
    ) -> Result<BookmarkListItem, AppError> {
        let (raw_url, normalized_url, url_hash, normalized_domain) = match &req.url {
            Some(u) if !u.trim().is_empty() => {
                let norm = normalize_url(u).map_err(AppError::BadRequest)?;
                (
                    Some(norm.raw_url),
                    Some(norm.normalized_url),
                    Some(norm.url_hash),
                    Some(norm.domain),
                )
            }
            _ => (None, None, None, None),
        };

        let title = req
            .title
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .or(normalized_domain.as_deref())
            .or(raw_url.as_deref())
            .unwrap_or("Untitled")
            .to_string();

        let item_type = req.item_type.unwrap_or_else(|| {
            if raw_url.is_some() {
                "LINK".to_string()
            } else {
                "NOTE".to_string()
            }
        });

        let bookmark_id = Uuid::now_v7();
        let client_req_id = req.client_request_id.unwrap_or_else(Uuid::now_v7);

        let user_edited_title = req.title.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        let user_edited_desc = req.description.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);

        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                bool,
                bool,
                bool,
                Option<Uuid>,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            r#"
            INSERT INTO bookmarks (
                id, user_id, folder_id, item_type, raw_url, normalized_url, url_hash, normalized_domain,
                title, user_edited_title, description, user_edited_desc, client_request_id, source, metadata_status, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'WEB', 'PENDING', NOW(), NOW())
            ON CONFLICT (user_id, client_request_id) DO UPDATE SET updated_at = bookmarks.updated_at
            RETURNING id, item_type, raw_url, normalized_domain, title, description, site_name,
                      thumbnail_url, favicon_url, is_pinned, is_favorite, is_archived, folder_id,
                      metadata_status, created_at, updated_at
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .bind(req.folder_id)
        .bind(&item_type)
        .bind(&raw_url)
        .bind(&normalized_url)
        .bind(&url_hash)
        .bind(&normalized_domain)
        .bind(&title)
        .bind(user_edited_title)
        .bind(req.description)
        .bind(user_edited_desc)
        .bind(client_req_id)
        .fetch_one(&self.pool)
        .await?;

        let actual_id = row.0;

        // Associate tags if provided
        let mut tag_summaries = Vec::new();
        if let Some(tag_names) = req.tags {
            for tag_name in tag_names {
                let trimmed = tag_name.trim();
                let slug = slugify(trimmed);
                if !slug.is_empty() {
                    let tag_row = sqlx::query_as::<_, (Uuid, String, String)>(
                        r#"
                        INSERT INTO tags (id, user_id, name, slug, created_at)
                        VALUES ($1, $2, $3, $4, NOW())
                        ON CONFLICT (user_id, slug) DO UPDATE SET name = EXCLUDED.name
                        RETURNING id, name, slug
                        "#,
                    )
                    .bind(Uuid::now_v7())
                    .bind(user_id)
                    .bind(trimmed)
                    .bind(&slug)
                    .fetch_one(&self.pool)
                    .await?;

                    sqlx::query(
                        r#"
                        INSERT INTO bookmark_tags (bookmark_id, tag_id)
                        VALUES ($1, $2)
                        ON CONFLICT DO NOTHING
                        "#,
                    )
                    .bind(actual_id)
                    .bind(tag_row.0)
                    .execute(&self.pool)
                    .await?;

                    tag_summaries.push(BookmarkTagSummary {
                        id: tag_row.0,
                        name: tag_row.1,
                        slug: tag_row.2,
                    });
                }
            }
        }

        // Enqueue background enrichment job to Redis
        if let (Some(u), Some(h), Some(mut redis_conn)) = (raw_url, url_hash, self.redis.clone()) {
            let job = EnrichmentJob {
                bookmark_id: actual_id,
                user_id,
                url: u,
                url_hash: h,
                attempt: 0,
            };
            if let Ok(job_json) = serde_json::to_string(&job) {
                let _: Result<(), redis::RedisError> = redis_conn.lpush(QUEUE_INTERACTIVE, job_json).await;
            }
        }

        Ok(BookmarkListItem {
            id: row.0,
            item_type: row.1,
            raw_url: row.2,
            normalized_domain: row.3,
            title: row.4,
            description: row.5,
            site_name: row.6,
            thumbnail_url: row.7,
            favicon_url: row.8,
            is_pinned: row.9,
            is_favorite: row.10,
            is_archived: row.11,
            folder_id: row.12,
            metadata_status: row.13,
            tags: tag_summaries,
            created_at: row.14,
            updated_at: row.15,
        })
    }

    pub async fn list_bookmarks(
        &self,
        user_id: Uuid,
        query: ListBookmarksQuery,
    ) -> Result<BookmarkListResponse, AppError> {
        let limit = query.limit.unwrap_or(20).clamp(1, 100);
        let is_archived = query.is_archived.unwrap_or(false);

        let mut qb = QueryBuilder::new(
            r#"
            SELECT b.id, b.item_type, b.raw_url, b.normalized_domain, b.title, b.description,
                   b.site_name, b.thumbnail_url, b.favicon_url, b.is_pinned, b.is_favorite,
                   b.is_archived, b.folder_id, b.metadata_status, b.created_at, b.updated_at
            FROM bookmarks b
            WHERE b.user_id = 
            "#,
        );
        qb.push_bind(user_id);
        qb.push(" AND b.deleted_at IS NULL AND b.is_archived = ");
        qb.push_bind(is_archived);

        if let Some(folder_id) = query.folder_id {
            qb.push(" AND b.folder_id = ");
            qb.push_bind(folder_id);
        }

        if let Some(is_fav) = query.is_favorite {
            if is_fav {
                qb.push(" AND b.is_favorite = TRUE");
            }
        }

        if let Some(domain) = query.domain {
            qb.push(" AND b.normalized_domain = ");
            qb.push_bind(domain);
        }

        if let Some(tag_slug) = query.tag {
            qb.push(" AND EXISTS (SELECT 1 FROM bookmark_tags bt JOIN tags t ON bt.tag_id = t.id WHERE bt.bookmark_id = b.id AND t.slug = ");
            qb.push_bind(tag_slug);
            qb.push(")");
        }

        // Keyset Cursor Pagination
        if let Some(cursor_str) = query.cursor {
            if let Some(cursor) = BookmarkCursor::decode(&cursor_str) {
                qb.push(" AND ((b.is_pinned < ");
                qb.push_bind(cursor.is_pinned);
                qb.push(") OR (b.is_pinned = ");
                qb.push_bind(cursor.is_pinned);
                qb.push(" AND b.created_at < ");
                qb.push_bind(cursor.created_at);
                qb.push(") OR (b.is_pinned = ");
                qb.push_bind(cursor.is_pinned);
                qb.push(" AND b.created_at = ");
                qb.push_bind(cursor.created_at);
                qb.push(" AND b.id < ");
                qb.push_bind(cursor.id);
                qb.push("))");
            }
        }

        qb.push(" ORDER BY b.is_pinned DESC, b.created_at DESC, b.id DESC LIMIT ");
        qb.push_bind(limit + 1);

        let rows = qb
            .build_query_as::<(
                Uuid,
                String,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                bool,
                bool,
                bool,
                Option<Uuid>,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
            )>()
            .fetch_all(&self.pool)
            .await?;

        let has_more = rows.len() as i64 > limit;
        let mut final_rows = rows;
        if has_more {
            final_rows.pop();
        }

        let bookmark_ids: Vec<Uuid> = final_rows.iter().map(|r| r.0).collect();

        // Batch fetch tags for these bookmarks
        let mut tags_by_bookmark: std::collections::HashMap<Uuid, Vec<BookmarkTagSummary>> =
            std::collections::HashMap::new();

        if !bookmark_ids.is_empty() {
            let tag_rows = sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
                r#"
                SELECT bt.bookmark_id, t.id, t.name, t.slug
                FROM bookmark_tags bt
                JOIN tags t ON bt.tag_id = t.id
                WHERE bt.bookmark_id = ANY($1)
                "#,
            )
            .bind(&bookmark_ids)
            .fetch_all(&self.pool)
            .await?;

            for (bm_id, tag_id, name, slug) in tag_rows {
                tags_by_bookmark.entry(bm_id).or_default().push(BookmarkTagSummary {
                    id: tag_id,
                    name,
                    slug,
                });
            }
        }

        let next_cursor = if has_more {
            final_rows.last().map(|r| {
                BookmarkCursor {
                    is_pinned: r.9,
                    created_at: r.14,
                    id: r.0,
                }
                .encode()
            })
        } else {
            None
        };

        let items = final_rows
            .into_iter()
            .map(|r| {
                let tags = tags_by_bookmark.remove(&r.0).unwrap_or_default();
                BookmarkListItem {
                    id: r.0,
                    item_type: r.1,
                    raw_url: r.2,
                    normalized_domain: r.3,
                    title: r.4,
                    description: r.5,
                    site_name: r.6,
                    thumbnail_url: r.7,
                    favicon_url: r.8,
                    is_pinned: r.9,
                    is_favorite: r.10,
                    is_archived: r.11,
                    folder_id: r.12,
                    metadata_status: r.13,
                    tags,
                    created_at: r.14,
                    updated_at: r.15,
                }
            })
            .collect();

        Ok(BookmarkListResponse {
            items,
            next_cursor,
            has_more,
        })
    }

    pub async fn get_bookmark(
        &self,
        user_id: Uuid,
        bookmark_id: Uuid,
    ) -> Result<BookmarkListItem, AppError> {
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                bool,
                bool,
                bool,
                Option<Uuid>,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            r#"
            SELECT id, item_type, raw_url, normalized_domain, title, description, site_name,
                   thumbnail_url, favicon_url, is_pinned, is_favorite, is_archived, folder_id,
                   metadata_status, created_at, updated_at
            FROM bookmarks
            WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Bookmark not found".to_string()))?;

        let tag_rows = sqlx::query_as::<_, (Uuid, String, String)>(
            r#"
            SELECT t.id, t.name, t.slug
            FROM bookmark_tags bt
            JOIN tags t ON bt.tag_id = t.id
            WHERE bt.bookmark_id = $1
            "#,
        )
        .bind(bookmark_id)
        .fetch_all(&self.pool)
        .await?;

        let tags = tag_rows
            .into_iter()
            .map(|t| BookmarkTagSummary {
                id: t.0,
                name: t.1,
                slug: t.2,
            })
            .collect();

        Ok(BookmarkListItem {
            id: row.0,
            item_type: row.1,
            raw_url: row.2,
            normalized_domain: row.3,
            title: row.4,
            description: row.5,
            site_name: row.6,
            thumbnail_url: row.7,
            favicon_url: row.8,
            is_pinned: row.9,
            is_favorite: row.10,
            is_archived: row.11,
            folder_id: row.12,
            metadata_status: row.13,
            tags,
            created_at: row.14,
            updated_at: row.15,
        })
    }

    pub async fn update_bookmark(
        &self,
        user_id: Uuid,
        bookmark_id: Uuid,
        req: UpdateBookmarkRequest,
    ) -> Result<BookmarkListItem, AppError> {
        let user_edited_title = req.title.is_some();
        let user_edited_desc = req.description.is_some();

        let _row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                bool,
                bool,
                bool,
                Option<Uuid>,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            r#"
            UPDATE bookmarks
            SET
                title = COALESCE($3, title),
                description = COALESCE($4, description),
                folder_id = CASE WHEN $5::boolean THEN $6 ELSE folder_id END,
                is_pinned = COALESCE($7, is_pinned),
                is_favorite = COALESCE($8, is_favorite),
                is_archived = COALESCE($9, is_archived),
                user_edited_title = CASE WHEN $10 THEN TRUE ELSE user_edited_title END,
                user_edited_desc = CASE WHEN $11 THEN TRUE ELSE user_edited_desc END,
                updated_at = NOW()
            WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL
            RETURNING id, item_type, raw_url, normalized_domain, title, description, site_name,
                      thumbnail_url, favicon_url, is_pinned, is_favorite, is_archived, folder_id,
                      metadata_status, created_at, updated_at
            "#,
        )
        .bind(bookmark_id)
        .bind(user_id)
        .bind(req.title)
        .bind(req.description)
        .bind(req.folder_id.is_some())
        .bind(req.folder_id)
        .bind(req.is_pinned)
        .bind(req.is_favorite)
        .bind(req.is_archived)
        .bind(user_edited_title)
        .bind(user_edited_desc)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Bookmark not found".to_string()))?;

        // Update tags if provided
        if let Some(tags) = req.tags {
            sqlx::query("DELETE FROM bookmark_tags WHERE bookmark_id = $1")
                .bind(bookmark_id)
                .execute(&self.pool)
                .await?;

            for tag_name in tags {
                let trimmed = tag_name.trim();
                let slug = slugify(trimmed);
                if !slug.is_empty() {
                    let tag_row = sqlx::query_as::<_, (Uuid,)>(
                        r#"
                        INSERT INTO tags (id, user_id, name, slug, created_at)
                        VALUES ($1, $2, $3, $4, NOW())
                        ON CONFLICT (user_id, slug) DO UPDATE SET name = EXCLUDED.name
                        RETURNING id
                        "#,
                    )
                    .bind(Uuid::now_v7())
                    .bind(user_id)
                    .bind(trimmed)
                    .bind(&slug)
                    .fetch_one(&self.pool)
                    .await?;

                    sqlx::query(
                        r#"
                        INSERT INTO bookmark_tags (bookmark_id, tag_id)
                        VALUES ($1, $2)
                        ON CONFLICT DO NOTHING
                        "#,
                    )
                    .bind(bookmark_id)
                    .bind(tag_row.0)
                    .execute(&self.pool)
                    .await?;
                }
            }
        }

        self.get_bookmark(user_id, bookmark_id).await
    }

    pub async fn delete_bookmark(&self, user_id: Uuid, bookmark_id: Uuid) -> Result<(), AppError> {
        let res = sqlx::query(
            "UPDATE bookmarks SET deleted_at = NOW() WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(bookmark_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if res.rows_affected() == 0 {
            return Err(AppError::NotFound("Bookmark not found".to_string()));
        }

        Ok(())
    }
}
