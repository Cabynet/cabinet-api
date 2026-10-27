use chrono::{DateTime, Utc};
use redis::AsyncCommands;
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

use crate::errors::AppError;
use crate::infra::http_client::SsrfSafeClient;
use crate::worker::queue::{EnrichmentJob, QUEUE_INTERACTIVE};
use super::dto::{
    BookmarkCursor, BookmarkListItem, BookmarkListResponse, BookmarkPreviewDto, BookmarkTagSummary,
    CreateBookmarkRequest, DuplicateCleanupResponse, DuplicateGroupDto, DuplicateItemDto,
    DuplicateReportResponse, LinkHealthCheckResponse, ListBookmarksQuery, UpdateBookmarkRequest,
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
        let req = req.resolved();
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
        let req = req.resolved();
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

    pub async fn get_previews(
        &self,
        user_id: Uuid,
        ids: Vec<Uuid>,
    ) -> Result<Vec<BookmarkPreviewDto>, AppError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let bounded_ids: Vec<Uuid> = ids.into_iter().take(50).collect();

        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"
            SELECT id, metadata_status, title, description, thumbnail_url, favicon_url, site_name
            FROM bookmarks
            WHERE id = ANY($1) AND user_id = $2 AND deleted_at IS NULL
            "#,
        )
        .bind(&bounded_ids)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let previews = rows
            .into_iter()
            .map(|r| BookmarkPreviewDto {
                id: r.0,
                metadata_status: r.1,
                title: r.2,
                description: r.3,
                thumbnail_url: r.4,
                favicon_url: r.5,
                site_name: r.6,
            })
            .collect();

        Ok(previews)
    }

    pub async fn get_duplicates(
        &self,
        user_id: Uuid,
    ) -> Result<DuplicateReportResponse, AppError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                Option<String>,
                Option<String>,
                String,
                DateTime<Utc>,
                Option<Vec<u8>>,
            ),
        >(
            r#"
            WITH dup_hashes AS (
                SELECT url_hash
                FROM bookmarks
                WHERE user_id = $1 AND deleted_at IS NULL AND url_hash IS NOT NULL
                GROUP BY url_hash
                HAVING COUNT(*) > 1
            )
            SELECT b.id, b.raw_url, b.normalized_domain, b.title, b.created_at, b.url_hash
            FROM bookmarks b
            JOIN dup_hashes dh ON b.url_hash = dh.url_hash
            WHERE b.user_id = $1 AND b.deleted_at IS NULL
            ORDER BY b.url_hash, b.created_at ASC, b.id ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let mut groups_map: std::collections::HashMap<Vec<u8>, DuplicateGroupDto> =
            std::collections::HashMap::new();

        for (id, raw_url, domain, title, created_at, url_hash) in rows {
            if let Some(hash) = url_hash {
                let entry = groups_map.entry(hash).or_insert_with(|| DuplicateGroupDto {
                    url: raw_url.unwrap_or_default(),
                    domain,
                    count: 0,
                    items: Vec::new(),
                });
                entry.count += 1;
                entry.items.push(DuplicateItemDto {
                    id,
                    title,
                    created_at,
                });
            }
        }

        let mut groups: Vec<DuplicateGroupDto> = groups_map.into_values().collect();
        groups.sort_by(|a, b| b.count.cmp(&a.count));

        let total_duplicate_groups = groups.len();
        let total_redundant_items = groups
            .iter()
            .map(|g| g.count.saturating_sub(1))
            .sum();

        Ok(DuplicateReportResponse {
            total_duplicate_groups,
            total_redundant_items,
            groups,
        })
    }

    pub async fn cleanup_duplicates(
        &self,
        user_id: Uuid,
    ) -> Result<DuplicateCleanupResponse, AppError> {
        // Keeps the earliest/oldest record per url_hash, soft-deletes redundant ones
        let res = sqlx::query(
            r#"
            WITH ranked AS (
                SELECT id,
                       ROW_NUMBER() OVER(PARTITION BY url_hash ORDER BY created_at ASC, id ASC) as rn
                FROM bookmarks
                WHERE user_id = $1 AND deleted_at IS NULL AND url_hash IS NOT NULL
            )
            UPDATE bookmarks
            SET deleted_at = NOW(), updated_at = NOW()
            WHERE id IN (
                SELECT id FROM ranked WHERE rn > 1
            ) AND user_id = $1
            "#,
        )
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        let cleaned_items = res.rows_affected() as usize;

        Ok(DuplicateCleanupResponse {
            cleaned_items,
            message: format!("Successfully cleaned up {} duplicate bookmarks", cleaned_items),
        })
    }

    pub async fn check_bookmark_health(
        &self,
        user_id: Uuid,
        bookmark_id: Uuid,
    ) -> Result<LinkHealthCheckResponse, AppError> {
        let row = sqlx::query_as::<_, (Option<String>,)>(
            "SELECT raw_url FROM bookmarks WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(bookmark_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Bookmark not found".to_string()))?;

        let raw_url = match row.0 {
            Some(u) if !u.trim().is_empty() => u,
            _ => {
                return Ok(LinkHealthCheckResponse {
                    id: bookmark_id,
                    url: None,
                    link_health: "OK".to_string(),
                    http_status_code: None,
                });
            }
        };

        let client = SsrfSafeClient::new()?;
        let (health, status_code) = match client.get_safe(&raw_url).await {
            Ok(resp) => {
                let code = resp.status().as_u16() as i16;
                if resp.status().is_success() {
                    ("OK".to_string(), Some(code))
                } else if resp.status().is_redirection() {
                    ("REDIRECT".to_string(), Some(code))
                } else {
                    ("BROKEN".to_string(), Some(code))
                }
            }
            Err(_) => ("BROKEN".to_string(), None),
        };

        sqlx::query(
            r#"
            UPDATE bookmarks
            SET link_health = $2, http_status_code = $3, last_checked_at = NOW(), updated_at = NOW()
            WHERE id = $1 AND user_id = $4
            "#,
        )
        .bind(bookmark_id)
        .bind(&health)
        .bind(status_code)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        Ok(LinkHealthCheckResponse {
            id: bookmark_id,
            url: Some(raw_url),
            link_health: health,
            http_status_code: status_code,
        })
    }
}
