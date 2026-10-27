use chrono::{DateTime, Utc};
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::bookmarks::dto::BookmarkTagSummary;
use super::dto::{SearchQuery, SearchResponse, SearchResultItem};

#[derive(Clone)]
pub struct SearchService {
    pub pool: PgPool,
}

impl SearchService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn search(
        &self,
        user_id: Uuid,
        query: SearchQuery,
    ) -> Result<SearchResponse, AppError> {
        let raw_q = query.q.trim();
        if raw_q.is_empty() {
            return Ok(SearchResponse {
                query: "".to_string(),
                total: 0,
                items: Vec::new(),
            });
        }

        // Limit query length to 200 chars
        let truncated_q = raw_q.chars().take(200).collect::<String>();

        // Tokenize words (split on any whitespace or non-alphanumeric delimiter, max 8 unique words)
        let words: Vec<String> = truncated_q
            .split(|c: char| !c.is_alphanumeric())
            .filter_map(|w| {
                let clean = w.trim().to_lowercase();
                if clean.is_empty() {
                    None
                } else {
                    Some(clean)
                }
            })
            .collect();

        if words.is_empty() {
            return Ok(SearchResponse {
                query: truncated_q,
                total: 0,
                items: Vec::new(),
            });
        }

        let mut unique_words = Vec::new();
        for w in words {
            if !unique_words.contains(&w) && unique_words.len() < 8 {
                unique_words.push(w);
            }
        }

        // Format tsquery: "word1:* & word2:* & ..."
        let tsquery_str = unique_words
            .iter()
            .map(|w| format!("{}:*", w))
            .collect::<Vec<_>>()
            .join(" & ");

        let limit = query.limit.unwrap_or(20).clamp(1, 50);

        let mut qb = QueryBuilder::new(
            r#"
            SELECT
                b.id,
                b.title,
                b.raw_url,
                b.normalized_domain,
                b.description,
                b.thumbnail_url,
                b.favicon_url,
                (ts_rank_cd(b.search_vector, to_tsquery('simple', 
            "#,
        );
        qb.push_bind(&tsquery_str);
        qb.push(r#")) * 1.5 + word_similarity("#);
        qb.push_bind(&truncated_q);
        qb.push(r#", b.title))::real as score, b.created_at FROM bookmarks b WHERE b.user_id = "#);
        qb.push_bind(user_id);
        qb.push(
            r#"
              AND b.deleted_at IS NULL
              AND b.is_archived = FALSE
              AND (
                b.search_vector @@ to_tsquery('simple', 
            "#,
        );
        qb.push_bind(&tsquery_str);
        qb.push(r#") OR word_similarity("#);
        qb.push_bind(&truncated_q);
        qb.push(r#", b.title) >= 0.35)"#);

        if let Some(folder_id) = query.folder_id {
            qb.push(" AND b.folder_id = ");
            qb.push_bind(folder_id);
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

        qb.push(" ORDER BY score DESC, b.created_at DESC, b.id DESC LIMIT ");
        qb.push_bind(limit);

        let rows = qb
            .build_query_as::<(
                Uuid,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                f32,
                DateTime<Utc>,
            )>()
            .fetch_all(&self.pool)
            .await?;

        let bookmark_ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();

        // Batch fetch tags
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

        let items = rows
            .into_iter()
            .map(|r| {
                let tags = tags_by_bookmark.remove(&r.0).unwrap_or_default();
                SearchResultItem {
                    id: r.0,
                    title: r.1,
                    raw_url: r.2,
                    normalized_domain: r.3,
                    description: r.4,
                    thumbnail_url: r.5,
                    favicon_url: r.6,
                    score: r.7,
                    tags,
                    created_at: r.8,
                }
            })
            .collect::<Vec<_>>();

        let total = items.len();

        Ok(SearchResponse {
            query: truncated_q,
            total,
            items,
        })
    }
}
