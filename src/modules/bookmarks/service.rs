use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use super::dto::{
    BookmarkListItem, BookmarkListResponse, BookmarkPreviewDto, CreateBookmarkRequest,
    DuplicateCleanupResponse, DuplicateGroupDto, DuplicateReportResponse, LinkHealthCheckResponse,
    ListBookmarksQuery, UpdateBookmarkRequest,
};
use super::normalizer::normalize_url;
use crate::errors::AppError;
use crate::infra::http_client::SsrfSafeClient;
use crate::modules::tags::service::slugify;
use crate::worker::queue::{EnrichmentJob, QUEUE_INTERACTIVE};

#[derive(Clone)]
pub struct BookmarksService {
    pub pool: PgPool,
    pub redis: Option<redis::aio::ConnectionManager>,
}

#[derive(Serialize, Deserialize)]
struct PageCursor {
    id: Uuid,
    pinned: bool,
    key: String,
    sort: String,
    /// Total computed on the first page and carried along, so later pages skip the count.
    #[serde(default)]
    total: i64,
}

impl BookmarksService {
    pub fn new(pool: PgPool, redis: Option<redis::aio::ConnectionManager>) -> Self {
        Self { pool, redis }
    }

    pub async fn check_folder(&self, user: Uuid, folder: Option<Uuid>) -> Result<(), AppError> {
        if let Some(id) = folder {
            let role = crate::modules::folders::collaboration::access(&self.pool, user, id).await?;
            if role == "VIEWER" {
                return Err(AppError::Forbidden("This folder is read-only".into()));
            }
        }
        Ok(())
    }

    pub async fn set_tags(
        tx: &mut Transaction<'_, Postgres>,
        user: Uuid,
        id: Uuid,
        names: &[String],
        replace: bool,
    ) -> Result<(), AppError> {
        if names.len() > 100 {
            return Err(AppError::BadRequest("At most 100 tags are allowed".into()));
        }

        if replace {
            sqlx::query("DELETE FROM bookmark_tags WHERE bookmark_id = $1")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }

        for name in names {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 64 {
                return Err(AppError::BadRequest(
                    "Tag names must contain 1–64 characters".into(),
                ));
            }

            let slug = slugify(name);
            if slug.is_empty() {
                return Err(AppError::BadRequest("Invalid tag name".into()));
            }

            let tag: Uuid = sqlx::query_scalar(
                r#"
                INSERT INTO tags (id, user_id, name, slug)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (user_id, slug) DO UPDATE SET name = tags.name
                RETURNING id
                "#,
            )
            .bind(Uuid::now_v7())
            .bind(user)
            .bind(name)
            .bind(&slug)
            .fetch_one(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO bookmark_tags (bookmark_id, tag_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(tag)
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }

    /// Adds the same tags to many bookmarks with two statements instead of 2 queries per (bookmark, tag).
    pub async fn add_tags_bulk(
        tx: &mut Transaction<'_, Postgres>,
        user: Uuid,
        ids: &[Uuid],
        names: &[String],
    ) -> Result<(), AppError> {
        if names.len() > 100 {
            return Err(AppError::BadRequest("At most 100 tags are allowed".into()));
        }

        let mut slugs: Vec<String> = Vec::new();
        let mut tag_names: Vec<String> = Vec::new();
        for name in names {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 64 {
                return Err(AppError::BadRequest("Tag names must contain 1–64 characters".into()));
            }
            let slug = slugify(name);
            if slug.is_empty() {
                return Err(AppError::BadRequest("Invalid tag name".into()));
            }
            // Two names with one slug would hit the same row twice in one upsert.
            if !slugs.contains(&slug) {
                slugs.push(slug);
                tag_names.push(name.to_string());
            }
        }
        if slugs.is_empty() {
            return Ok(());
        }

        let tag_ids: Vec<Uuid> = sqlx::query_scalar(
            r#"
            INSERT INTO tags (id, user_id, name, slug)
            SELECT gen_random_uuid(), $1, n, s FROM unnest($2::text[], $3::text[]) AS t(n, s)
            ON CONFLICT (user_id, slug) DO UPDATE SET name = tags.name
            RETURNING id
            "#,
        )
        .bind(user)
        .bind(&tag_names)
        .bind(&slugs)
        .fetch_all(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO bookmark_tags (bookmark_id, tag_id)
            SELECT b, t FROM unnest($1::uuid[]) AS b CROSS JOIN unnest($2::uuid[]) AS t
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(ids)
        .bind(&tag_ids)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub async fn create_bookmark(
        &self,
        user: Uuid,
        req: CreateBookmarkRequest,
    ) -> Result<BookmarkListItem, AppError> {
        self.check_folder(user, req.folder_id).await?;

        let norm = req
            .url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .map(normalize_url)
            .transpose()
            .map_err(AppError::BadRequest)?;

        let kind = req
            .item_type
            .as_deref()
            .unwrap_or(if norm.is_some() { "LINK" } else { "NOTE" });
        let kind = if kind == "BOOKMARK" { "LINK" } else { kind };

        if !["LINK", "NOTE", "DRAWING", "FILE"].contains(&kind) {
            return Err(AppError::BadRequest("Unsupported item type".into()));
        }

        let title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(norm.as_ref().map(|n| n.domain.as_str()))
            .unwrap_or("Untitled");

        if title.chars().count() > 2000 {
            return Err(AppError::BadRequest("Title is too long".into()));
        }

        let user_edited_title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|s| {
                let lower = s.to_ascii_lowercase();
                !lower.starts_with("just a moment")
                    && !lower.starts_with("attention required")
                    && !lower.starts_with("access denied")
                    && !lower.contains("community on reddit")
                    && !lower.contains("komunitas")
                    && Some(s) != norm.as_ref().map(|n| n.domain.as_str())
                    && Some(s) != norm.as_ref().map(|n| n.raw_url.as_str())
            });

        let has_metadata = req
            .title
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && req
                .description
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            && req
                .thumbnail_url
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty());

        let initial_status = if kind == "LINK" && !has_metadata {
            "PENDING"
        } else {
            "COMPLETED"
        };

        let requested_id = Uuid::now_v7();
        let mut tx = self.pool.begin().await?;

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO bookmarks (
                id, user_id, folder_id, item_type, raw_url, normalized_url, url_hash, normalized_domain,
                title, user_edited_title, description, user_edited_desc, client_request_id,
                metadata_status, note_document, notes_plain, drawing, thumbnail_url, favicon_url, content_type
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
            ON CONFLICT (user_id, client_request_id) DO UPDATE SET updated_at = bookmarks.updated_at
            RETURNING id
            "#,
        )
        .bind(requested_id)
        .bind(user)
        .bind(req.folder_id)
        .bind(kind)
        .bind(norm.as_ref().map(|n| &n.raw_url))
        .bind(norm.as_ref().map(|n| &n.normalized_url))
        .bind(norm.as_ref().map(|n| &n.url_hash))
        .bind(norm.as_ref().map(|n| &n.domain))
        .bind(title)
        .bind(user_edited_title)
        .bind(&req.description)
        .bind(req.description.as_deref().is_some_and(|s| !s.trim().is_empty()))
        .bind(req.client_request_id.unwrap_or_else(Uuid::now_v7))
        .bind(initial_status)
        .bind(req.note_document)
        .bind(req.notes_plain)
        .bind(req.drawing)
        .bind(req.thumbnail_url)
        .bind(req.favicon_url)
        .bind(req.content_type)
        .fetch_one(&mut *tx)
        .await?;

        // Replayed creates must not mutate an already acknowledged item or its tags.
        if id == requested_id {
            if let Some(names) = &req.tags {
                Self::set_tags(&mut tx, user, id, names, false).await?;
            }
            // Link an uploaded file to the bookmark that points at it, so deleting the bookmark can release it.
            if let Some(n) = &norm {
                sqlx::query(
                    r#"
                    UPDATE media_assets SET bookmark_id = $1
                    WHERE user_id = $2 AND bookmark_id IS NULL AND status = 'READY'
                      AND right($3, length(object_key)) = object_key
                    "#,
                )
                .bind(id)
                .bind(user)
                .bind(&n.raw_url)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;

        if kind == "LINK" && !has_metadata {
            self.enqueue(user, id).await?;
        }

        self.get_bookmark(user, id).await
    }

    pub async fn enqueue(&self, user: Uuid, id: Uuid) -> Result<(), AppError> {
        let row: Option<(String, Vec<u8>)> = sqlx::query_as(
            r#"
            SELECT raw_url, url_hash
            FROM bookmarks
            WHERE id = $1
              AND user_id = $2
              AND metadata_status = 'PENDING'
              AND deleted_at IS NULL
              AND raw_url IS NOT NULL
            "#,
        )
        .bind(id)
        .bind(user)
        .fetch_optional(&self.pool)
        .await?;

        if let (Some((url, url_hash)), Some(mut redis)) = (row, self.redis.clone()) {
            let job = EnrichmentJob {
                bookmark_id: id,
                user_id: user,
                url,
                url_hash,
                attempt: 0,
            };
            let encoded = serde_json::to_string(&job).map_err(|e| AppError::Internal(e.into()))?;
            // Database recovery is authoritative if Redis is unavailable.
            let _: Result<(), _> = redis.lpush(QUEUE_INTERACTIVE, encoded).await;
        }

        crate::worker::queue::wake_workers();
        Ok(())
    }

    pub async fn list_bookmarks(
        &self,
        user: Uuid,
        q: ListBookmarksQuery,
    ) -> Result<BookmarkListResponse, AppError> {
        let limit = q.limit.unwrap_or(20).clamp(1, 100);
        let sort = q.sort.as_deref().unwrap_or("newest");
        let (key, asc) = match sort {
            "custom" => ("lpad((b.position::bigint + 2147483648)::text, 12, '0')", true),
            "title" | "title-asc" => ("lower(b.title)", true),
            "domain" => ("coalesce(b.normalized_domain, '')", true),
            "oldest" | "created-asc" => ("b.created_at::text", true),
            "updated" | "updated-desc" => ("b.updated_at::text", false),
            "newest" | "created-desc" => ("b.created_at::text", false),
            "relevance" => (
                "lpad((ts_rank_cd(b.search_vector, websearch_to_tsquery('simple', coalesce($1, '')))*1000000)::bigint::text, 20, '0')",
                false,
            ),
            _ => return Err(AppError::BadRequest("Unsupported sort".into())),
        };

        let search = q.search.as_deref().unwrap_or("").trim();
        let in_shared_folder = q.folder_id.is_some() && !q.trash.unwrap_or(false);
        let global_search = q.folder_id.is_none()
            && !q.trash.unwrap_or(false)
            && q.is_favorite.is_none()
            && q.is_archived.is_none()
            && q.tag.is_none()
            && q.status.is_none()
            && q.item_type.is_none()
            && q.content_type.is_none()
            && !search.is_empty();

        let mut qb = QueryBuilder::new("WITH filtered AS (SELECT b.*, ");
        // Keep the search text as the first bind for the relevance expression.
        qb.push(" ");
        qb.push_bind(search.to_owned());
        qb.push("::text AS query_text, ")
            .push(key)
            .push(" AS sort_key FROM bookmarks b WHERE ");

        if in_shared_folder || global_search {
            qb.push("(b.user_id = ")
                .push_bind(user)
                .push(" OR b.folder_id IN (SELECT folder_id FROM folder_members WHERE user_id = $2) OR b.folder_id IN (SELECT id FROM folders WHERE user_id = $2))");
        } else {
            qb.push("b.user_id = ")
                .push_bind(user);
        }

        if q.trash.unwrap_or(false) {
            qb.push(" AND b.deleted_at IS NOT NULL AND b.user_id = $2");
        } else {
            qb.push(" AND b.deleted_at IS NULL AND b.is_archived = ")
                .push_bind(q.is_archived.unwrap_or(false));
        }

        if let Some(status) = q.status {
            qb.push(" AND b.link_health = ").push_bind(status);
        }
        if let Some(folder) = q.folder_id {
            qb.push(" AND b.folder_id = ").push_bind(folder);
        }
        if let Some(favorite) = q.is_favorite {
            qb.push(" AND b.is_favorite = ").push_bind(favorite);
        }
        if let Some(domain) = q.domain {
            qb.push(" AND b.normalized_domain = ")
                .push_bind(domain.trim_start_matches("www.").to_owned());
        }
        if let Some(tag) = q.tag {
            qb.push(" AND EXISTS (SELECT 1 FROM bookmark_tags bt JOIN tags t ON t.id = bt.tag_id WHERE bt.bookmark_id = b.id AND (t.slug = ")
                .push_bind(tag.clone())
                .push(" OR t.id::text = ")
                .push_bind(tag)
                .push("))");
        }
        if let Some(kind) = q.item_type {
            qb.push(" AND b.item_type = ")
                .push_bind(if kind == "BOOKMARK" { "LINK".into() } else { kind });
        }
        if let Some(kind) = q.content_type {
            qb.push(" AND b.content_type = ").push_bind(kind);
        }
        if let Some(date) = q.added_after {
            qb.push(" AND b.created_at >= ").push_bind(date);
        }
        if let Some(date) = q.added_before {
            qb.push(" AND b.created_at <= ").push_bind(date);
        }
        if !search.is_empty() {
            qb.push(" AND (b.search_vector @@ websearch_to_tsquery('simple', ")
                .push_bind(search.to_owned())
                .push(") OR b.title ILIKE ")
                .push_bind(format!("%{}%", search))
                .push(" OR word_similarity(")
                .push_bind(search.to_lowercase())
                .push(", lower(b.title)) > 0.3)");
        }

        let cursor: Option<PageCursor> = match q.cursor {
            Some(encoded) => {
                use base64::Engine;
                let cursor: PageCursor = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(encoded)
                    .ok()
                    .and_then(|v| serde_json::from_slice(&v).ok())
                    .ok_or_else(|| AppError::BadRequest("Invalid cursor".into()))?;
                if cursor.sort != sort {
                    return Err(AppError::BadRequest("Cursor sort does not match".into()));
                }
                Some(cursor)
            }
            None => None,
        };

        qb.push(") SELECT ");
        if q.summary.unwrap_or(false) {
            // Summaries skip the heavy note/drawing documents; notes_plain keeps a text preview.
            qb.push("(to_jsonb(b) - 'drawing' - 'note_document' - 'search_vector' - 'url_hash') || jsonb_build_object('notes_plain', left(b.notes_plain, 1000), 'drawing', null)");
        } else {
            qb.push("to_jsonb(b) - 'search_vector' - 'url_hash'");
        }
        qb.push(
            r#"
            || jsonb_build_object(
                'access', CASE
                    WHEN b.user_id = $2 THEN 'owner'
                    WHEN EXISTS (SELECT 1 FROM folder_members m WHERE m.folder_id = b.folder_id AND m.user_id = $2 AND m.role = 'EDITOR') THEN 'editor'
                    ELSE 'viewer'
                END,
                'tags', coalesce(
                    (SELECT jsonb_agg(jsonb_build_object('id', t.id, 'name', t.name, 'slug', t.slug) ORDER BY t.name)
                     FROM bookmark_tags bt
                     JOIN tags t ON t.id = bt.tag_id
                     WHERE bt.bookmark_id = b.id AND t.user_id = $2),
                    '[]'::jsonb
                )
            ) AS payload, "#,
        );
        // The window count sees the whole filtered set (it runs before LIMIT), but only the first page needs it.
        match &cursor {
            None => qb.push("count(*) OVER ()"),
            Some(c) => qb.push_bind(c.total),
        };
        qb.push(" AS total FROM filtered b WHERE TRUE");

        if let Some(cursor) = cursor {
            qb.push(" AND (b.is_pinned < ")
                .push_bind(cursor.pinned)
                .push(" OR (b.is_pinned = ")
                .push_bind(cursor.pinned)
                .push(" AND (b.sort_key ")
                .push(if asc { ">" } else { "<" })
                .push(" ")
                .push_bind(cursor.key.clone())
                .push(" OR (b.sort_key = ")
                .push_bind(cursor.key)
                .push(" AND b.id ")
                .push(if asc { ">" } else { "<" })
                .push(" ")
                .push_bind(cursor.id)
                .push("))))");
        }

        qb.push(" ORDER BY b.is_pinned DESC, b.sort_key ")
            .push(if asc { "ASC" } else { "DESC" })
            .push(", b.id ")
            .push(if asc { "ASC" } else { "DESC" })
            .push(" LIMIT ")
            .push_bind(limit + 1);

        let mut rows: Vec<(Value, i64)> = qb.build_query_as().fetch_all(&self.pool).await?;
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.pop();
        }

        let total_count = rows.first().map(|r| r.1).unwrap_or(0);
        let next_cursor = if has_more {
            rows.last().map(|r| {
                use base64::Engine;
                let c = PageCursor {
                    id: serde_json::from_value(r.0["id"].clone()).unwrap(),
                    pinned: r.0["is_pinned"].as_bool().unwrap_or(false),
                    key: r.0["sort_key"].as_str().unwrap_or("").into(),
                    sort: sort.into(),
                    total: total_count,
                };
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(&c).unwrap())
            })
        } else {
            None
        };

        let items = rows
            .into_iter()
            .map(|r| serde_json::from_value(crate::infra::camel::camelize(r.0)).map_err(|e| AppError::Internal(e.into())))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(BookmarkListResponse {
            items,
            next_cursor,
            has_more,
            total_count,
        })
    }

    pub async fn get_bookmark(&self, user: Uuid, id: Uuid) -> Result<BookmarkListItem, AppError> {
        let value: Option<Value> = sqlx::query_scalar(
            r#"
            SELECT to_jsonb(b)
                || jsonb_build_object(
                    'access', CASE
                        WHEN b.user_id = $2 THEN 'owner'
                        WHEN m.role = 'EDITOR' THEN 'editor'
                        ELSE 'viewer'
                    END,
                    'tags', coalesce(
                        (SELECT jsonb_agg(jsonb_build_object('id', t.id, 'name', t.name, 'slug', t.slug) ORDER BY t.name)
                         FROM bookmark_tags bt
                         JOIN tags t ON t.id = bt.tag_id
                         WHERE bt.bookmark_id = b.id AND b.user_id = $2),
                        '[]'::jsonb
                    )
                )
            FROM bookmarks b
            LEFT JOIN folder_members m ON m.folder_id = b.folder_id AND m.user_id = $2
            WHERE b.id = $1
              AND (b.user_id = $2 OR m.user_id IS NOT NULL OR b.folder_id IN (SELECT id FROM folders WHERE user_id = $2))
              AND b.deleted_at IS NULL
            "#,
        )
        .bind(id)
        .bind(user)
        .fetch_optional(&self.pool)
        .await?;

        serde_json::from_value(crate::infra::camel::camelize(value.ok_or_else(|| AppError::NotFound("Bookmark not found".into()))?))
            .map_err(|e| AppError::Internal(e.into()))
    }

    pub async fn update_bookmark(
        &self,
        user: Uuid,
        id: Uuid,
        req: UpdateBookmarkRequest,
    ) -> Result<BookmarkListItem, AppError> {
        let item = self.get_bookmark(user, id).await?;
        if item.access == "viewer" {
            return Err(AppError::Forbidden("This item is read-only".into()));
        }

        if item.user_id != user
            && (req.folder_id.is_some()
                || req.set_folder.unwrap_or(false)
                || req.tags.is_some()
                || req.is_pinned.is_some()
                || req.is_favorite.is_some()
                || req.is_archived.is_some()
                || req.notes_plain.is_some()
                || req.thumbnail_url.is_some())
        {
            return Err(AppError::Forbidden(
                "Personal fields belong to the item owner".into(),
            ));
        }

        self.check_folder(user, req.folder_id).await?;
        let actor = user;
        let user = item.user_id;
        let norm = req
            .url
            .as_deref()
            .map(normalize_url)
            .transpose()
            .map_err(AppError::BadRequest)?;

        let mut tx = self.pool.begin().await?;

        if req.notes_plain.is_some() {
            let (revision, save): (i32, Option<Uuid>) = sqlx::query_as(
                r#"
                SELECT note_revision, last_note_save_id
                FROM bookmarks
                WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL
                FOR UPDATE
                "#,
            )
            .bind(id)
            .bind(user)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::NotFound("Bookmark not found".into()))?;

            if req.save_id.is_some() && req.save_id == save {
                tx.rollback().await?;
                return self.get_bookmark(actor, id).await;
            }

            if req.base_revision != Some(revision) {
                return Err(AppError::Conflict(
                    "Note revision changed; reload before saving".into(),
                ));
            }
        }

        let result = sqlx::query(
            r#"
            UPDATE bookmarks
            SET title = coalesce($3, title),
                description = coalesce($4, description),
                folder_id = CASE WHEN $16 THEN $5 ELSE coalesce($5, folder_id) END,
                is_pinned = coalesce($6, is_pinned),
                is_favorite = coalesce($7, is_favorite),
                is_archived = coalesce($8, is_archived),
                user_edited_title = user_edited_title OR $3 IS NOT NULL,
                user_edited_desc = user_edited_desc OR $4 IS NOT NULL,
                thumbnail_url = coalesce($9, thumbnail_url),
                raw_url = coalesce($10, raw_url),
                normalized_url = coalesce($11, normalized_url),
                url_hash = coalesce($12, url_hash),
                normalized_domain = coalesce($13, normalized_domain),
                metadata_status = CASE
                    WHEN $10 IS NOT NULL AND $10 IS DISTINCT FROM raw_url THEN 'PENDING'
                    ELSE metadata_status
                END,
                metadata_attempts = CASE
                    WHEN $10 IS NOT NULL AND $10 IS DISTINCT FROM raw_url THEN 0
                    ELSE metadata_attempts
                END,
                notes_plain = coalesce($14, notes_plain),
                note_revision = note_revision + CASE WHEN $14 IS NOT NULL THEN 1 ELSE 0 END,
                last_note_save_id = CASE WHEN $14 IS NOT NULL THEN $15 ELSE last_note_save_id END,
                updated_at = now()
            WHERE id = $1
              AND user_id = $2
              AND deleted_at IS NULL
              AND (user_id = $17 OR EXISTS (SELECT 1 FROM folder_members m WHERE m.folder_id = bookmarks.folder_id AND m.user_id = $17 AND m.role = 'EDITOR'))
            "#,
        )
        .bind(id)
        .bind(user)
        .bind(req.title)
        .bind(req.description)
        .bind(req.folder_id)
        .bind(req.is_pinned)
        .bind(req.is_favorite)
        .bind(req.is_archived)
        .bind(req.thumbnail_url)
        .bind(norm.as_ref().map(|n| &n.raw_url))
        .bind(norm.as_ref().map(|n| &n.normalized_url))
        .bind(norm.as_ref().map(|n| &n.url_hash))
        .bind(norm.as_ref().map(|n| &n.domain))
        .bind(req.notes_plain)
        .bind(req.save_id)
        .bind(req.set_folder.unwrap_or(false))
        .bind(actor)
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(AppError::NotFound("Bookmark not found".into()));
        }

        if let Some(names) = req.tags {
            Self::set_tags(&mut tx, user, id, &names, true).await?;
        }

        tx.commit().await?;

        if norm.is_some() {
            self.enqueue(user, id).await?;
        }

        self.get_bookmark(actor, id).await
    }

    pub async fn delete_bookmark(&self, user: Uuid, id: Uuid) -> Result<(), AppError> {
        let item = self.get_bookmark(user, id).await?;
        if item.access == "viewer" {
            return Err(AppError::Forbidden("This item is read-only".into()));
        }

        let actor = user;
        let user = item.user_id;

        let r = sqlx::query(
            r#"
            UPDATE bookmarks
            SET deleted_at = now(), updated_at = now()
            WHERE id = $1
              AND user_id = $2
              AND deleted_at IS NULL
              AND (user_id = $3 OR EXISTS (SELECT 1 FROM folder_members m WHERE m.folder_id = bookmarks.folder_id AND m.user_id = $3 AND m.role = 'EDITOR'))
            "#,
        )
        .bind(id)
        .bind(user)
        .bind(actor)
        .execute(&self.pool)
        .await?;

        if r.rows_affected() == 0 {
            return Err(AppError::NotFound("Bookmark not found".into()));
        }
        Ok(())
    }

    pub async fn get_previews(
        &self,
        user: Uuid,
        ids: Vec<Uuid>,
    ) -> Result<Vec<BookmarkPreviewDto>, AppError> {
        if ids.len() > 100 {
            return Err(AppError::BadRequest("At most 100 preview IDs".into()));
        }

        let rows: Vec<Value> = sqlx::query_scalar(
            r#"
            SELECT to_jsonb(b)
            FROM bookmarks b
            WHERE b.id = ANY($1)
              AND (b.user_id = $2 OR EXISTS (SELECT 1 FROM folder_members m WHERE m.folder_id = b.folder_id AND m.user_id = $2) OR b.folder_id IN (SELECT id FROM folders WHERE user_id = $2))
              AND b.deleted_at IS NULL
            "#,
        )
        .bind(ids)
        .bind(user)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|v| serde_json::from_value(crate::infra::camel::camelize(v)).map_err(|e| AppError::Internal(e.into())))
            .collect()
    }

    pub async fn get_duplicates(&self, user: Uuid) -> Result<DuplicateReportResponse, AppError> {
        let rows: Vec<(Value, Vec<u8>)> = sqlx::query_as(
            r#"
            SELECT to_jsonb(b)
                || jsonb_build_object(
                    'access', 'owner',
                    'tags', coalesce(
                        (SELECT jsonb_agg(jsonb_build_object('id', t.id, 'name', t.name, 'slug', t.slug))
                         FROM bookmark_tags bt
                         JOIN tags t ON t.id = bt.tag_id
                         WHERE bt.bookmark_id = b.id),
                        '[]'::jsonb
                    )
                ), b.url_hash
            FROM bookmarks b
            WHERE b.user_id = $1
              AND b.deleted_at IS NULL
              AND b.url_hash IN (
                  SELECT url_hash
                  FROM bookmarks
                  WHERE user_id = $1 AND deleted_at IS NULL AND url_hash IS NOT NULL
                  GROUP BY url_hash
                  HAVING count(*) > 1
              )
            ORDER BY b.created_at, b.id
            "#,
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;

        let mut groups = std::collections::HashMap::new();
        for (value, hash) in rows {
            let item: BookmarkListItem =
                serde_json::from_value(crate::infra::camel::camelize(value)).map_err(|e| AppError::Internal(e.into()))?;
            let g = groups.entry(hash).or_insert_with(|| DuplicateGroupDto {
                url: item.raw_url.clone().unwrap_or_default(),
                domain: item.normalized_domain.clone(),
                count: 0,
                items: vec![],
            });
            g.count += 1;
            g.items.push(item);
        }

        let mut groups: Vec<_> = groups.into_values().collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.count));

        Ok(DuplicateReportResponse {
            total_duplicate_groups: groups.len(),
            total_redundant_items: groups.iter().map(|g| g.count - 1).sum(),
            groups,
        })
    }

    pub async fn cleanup_duplicates(&self, user: Uuid) -> Result<DuplicateCleanupResponse, AppError> {
        let r = sqlx::query(
            r#"
            WITH ranked AS (
                SELECT id, row_number() OVER (PARTITION BY url_hash ORDER BY created_at, id) rn
                FROM bookmarks
                WHERE user_id = $1 AND deleted_at IS NULL AND url_hash IS NOT NULL
            )
            UPDATE bookmarks
            SET deleted_at = now(), updated_at = now()
            WHERE user_id = $1
              AND id IN (SELECT id FROM ranked WHERE rn > 1)
            "#,
        )
        .bind(user)
        .execute(&self.pool)
        .await?;

        Ok(DuplicateCleanupResponse {
            cleaned_items: r.rows_affected() as usize,
            message: "Duplicates moved to Trash".into(),
        })
    }

    pub async fn check_bookmark_health(
        &self,
        user: Uuid,
        id: Uuid,
    ) -> Result<LinkHealthCheckResponse, AppError> {
        let item = self.get_bookmark(user, id).await?;
        let (health, code) = if let Some(url) = &item.raw_url {
            match SsrfSafeClient::new()?.get_safe(url).await {
                Ok(r) => {
                    let code = r.status().as_u16() as i16;
                    let health = if r.status().is_success() {
                        "OK"
                    } else if code == 404 || code == 410 {
                        "BROKEN"
                    } else {
                        "UNKNOWN"
                    };
                    (health, Some(code))
                }
                Err(_) => ("UNKNOWN", None),
            }
        } else {
            ("OK", None)
        };

        sqlx::query(
            r#"
            UPDATE bookmarks
            SET link_health = $3, http_status_code = $4, last_checked_at = now()
            WHERE id = $1
              AND user_id = $2
              AND raw_url IS NOT DISTINCT FROM $5
              AND deleted_at IS NULL
            "#,
        )
        .bind(id)
        .bind(user)
        .bind(health)
        .bind(code)
        .bind(&item.raw_url)
        .execute(&self.pool)
        .await?;

        Ok(LinkHealthCheckResponse {
            id,
            url: item.raw_url,
            link_health: health.into(),
            http_status_code: code,
        })
    }
}
