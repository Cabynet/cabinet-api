use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::service::BookmarksService;
use crate::modules::responses::{AffectedResponse, EmbeddableResponse, IdResponse, LinkHealthSummary, ScrapeResponse, TaskQueuedResponse};
use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;

pub fn router() -> Router<PgPool> {
    Router::new()
        .route("/bulk/:action", post(bulk))
        .route("/reorder", post(reorder))
        .route("/trash/empty", delete(empty_trash))
        .route("/:id/restore", post(restore))
        .route("/:id/permanent", delete(permanent))
        .route("/:id/drawing", put(save_drawing))
        .route("/health", get(health))
        .route("/health/check", post(check_health))
        .route("/scrape", get(scrape))
        .route("/embeddable", get(embeddable))
        .route("/proxy-page", get(proxy_page))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkRequest {
    pub ids: Vec<Uuid>,
    #[serde(alias = "folder_id")]
    pub folder_id: Option<Uuid>,
    #[serde(alias = "is_archived")]
    pub is_archived: Option<bool>,
    pub tags: Option<Vec<String>>,
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/bulk/{action}",
    params(
        ("action" = String, Path, description = "trash | restore | permanent-delete | archive | move | tags"),
    ),
    responses(
        (status = 200, description = "OK", body = AffectedResponse)
    ),
    tag = "bookmarks"
)]
pub async fn bulk(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(action): Path<String>,
    Json(req): Json<BulkRequest>,
) -> Result<Json<crate::modules::responses::AffectedResponse>, AppError> {
    if req.ids.is_empty() || req.ids.len() > 1000 {
        return Err(AppError::BadRequest("Choose between 1 and 1000 items".into()));
    }

    let svc = BookmarksService::new(pool.clone(), None);
    if action == "move" {
        svc.check_folder(user.id, req.folder_id).await?;
    }

    let mut tx = pool.begin().await?;
    let mut purged_keys: Vec<String> = Vec::new();
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM bookmarks WHERE id = ANY($1) AND user_id = $2 FOR UPDATE",
    )
    .bind(&req.ids)
    .bind(user.id)
    .fetch_all(&mut *tx)
    .await?;

    let unique: std::collections::HashSet<_> = req.ids.iter().collect();
    if ids.len() != unique.len() {
        return Err(AppError::NotFound("Some items are no longer available".into()));
    }

    match action.as_str() {
        "trash" => {
            sqlx::query(
                "UPDATE bookmarks SET deleted_at = now(), updated_at = now() WHERE id = ANY($1) AND user_id = $2",
            )
            .bind(&ids)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
        }
        "restore" => {
            sqlx::query(
                "UPDATE bookmarks SET deleted_at = null, updated_at = now() WHERE id = ANY($1) AND user_id = $2",
            )
            .bind(&ids)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
        }
        "permanent-delete" => {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM bookmarks WHERE id = ANY($1) AND user_id = $2 AND deleted_at IS NOT NULL",
            )
            .bind(&ids)
            .bind(user.id)
            .fetch_one(&mut *tx)
            .await?;

            if count != ids.len() as i64 {
                return Err(AppError::BadRequest(
                    "Move items to Trash before deleting permanently".into(),
                ));
            }

            let (_, keys) = crate::modules::media::service::purge_trashed(&mut tx, user.id, Some(ids.clone())).await?;
            purged_keys = keys;
        }
        "archive" => {
            let is_archived = req
                .is_archived
                .ok_or_else(|| AppError::BadRequest("is_archived is required".into()))?;

            sqlx::query(
                "UPDATE bookmarks SET is_archived = $3, updated_at = now() WHERE id = ANY($1) AND user_id = $2 AND deleted_at IS NULL",
            )
            .bind(&ids)
            .bind(user.id)
            .bind(is_archived)
            .execute(&mut *tx)
            .await?;
        }
        "move" => {
            sqlx::query(
                "UPDATE bookmarks SET folder_id = $3, updated_at = now() WHERE id = ANY($1) AND user_id = $2 AND deleted_at IS NULL",
            )
            .bind(&ids)
            .bind(user.id)
            .bind(req.folder_id)
            .execute(&mut *tx)
            .await?;
        }
        "tags" => {
            let names = req
                .tags
                .ok_or_else(|| AppError::BadRequest("tags are required".into()))?;
            BookmarksService::add_tags_bulk(&mut tx, user.id, &ids, &names).await?;
        }
        _ => return Err(AppError::NotFound("Unknown bulk action".into())),
    }

    tx.commit().await?;
    crate::modules::media::service::delete_objects(purged_keys).await;
    Ok(Json(AffectedResponse { affected: ids.len() as u64 }))
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/{id}/restore",
    params(
        ("id" = Uuid, Path, description = "Resource ID"),
    ),
    responses(
        (status = 200, description = "OK", body = AffectedResponse)
    ),
    tag = "bookmarks"
)]
pub async fn restore(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::AffectedResponse>, AppError> {
    bulk(
        State(pool),
        CurrentUser(user),
        Path("restore".into()),
        Json(BulkRequest {
            ids: vec![id],
            folder_id: None,
            is_archived: None,
            tags: None,
        }),
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/v1/bookmarks/{id}/permanent",
    params(
        ("id" = Uuid, Path, description = "Resource ID"),
    ),
    responses(
        (status = 200, description = "OK", body = AffectedResponse)
    ),
    tag = "bookmarks"
)]
pub async fn permanent(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::modules::responses::AffectedResponse>, AppError> {
    bulk(
        State(pool),
        CurrentUser(user),
        Path("permanent-delete".into()),
        Json(BulkRequest {
            ids: vec![id],
            folder_id: None,
            is_archived: None,
            tags: None,
        }),
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/v1/bookmarks/trash/empty",
    responses(
        (status = 200, description = "OK", body = AffectedResponse)
    ),
    tag = "bookmarks"
)]
pub async fn empty_trash(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<crate::modules::responses::AffectedResponse>, AppError> {
    let mut tx = pool.begin().await?;
    let (affected, keys) = crate::modules::media::service::purge_trashed(&mut tx, user.id, None).await?;
    tx.commit().await?;
    crate::modules::media::service::delete_objects(keys).await;

    Ok(Json(AffectedResponse { affected }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReorderRequest {
    ids: Vec<Uuid>,
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/reorder",
    responses(
        (status = 200, description = "OK", body = AffectedResponse)
    ),
    tag = "bookmarks"
)]
pub async fn reorder(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<ReorderRequest>,
) -> Result<Json<crate::modules::responses::AffectedResponse>, AppError> {
    if req.ids.len() > 1000 {
        return Err(AppError::BadRequest("At most 1000 items can be reordered".into()));
    }

    let unique: std::collections::HashSet<_> = req.ids.iter().collect();
    if unique.len() != req.ids.len() {
        return Err(AppError::BadRequest("Duplicate reorder IDs".into()));
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;

    let slots: Vec<i32> = sqlx::query_scalar(
        "SELECT position FROM bookmarks WHERE id = ANY($1) AND user_id = $2 AND deleted_at IS NULL ORDER BY position FOR UPDATE",
    )
    .bind(&req.ids)
    .bind(user.id)
    .fetch_all(&mut *tx)
    .await?;

    if slots.len() != req.ids.len() {
        return Err(AppError::NotFound("Item not found".into()));
    }

    // Reuse only these slots: unloaded items keep their position and relative order.
    sqlx::query(
        r#"
        UPDATE bookmarks b SET position = v.position, updated_at = now()
        FROM unnest($1::uuid[], $2::int[]) AS v(id, position)
        WHERE b.id = v.id AND b.user_id = $3
        "#,
    )
    .bind(&req.ids)
    .bind(&slots)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(AffectedResponse { affected: req.ids.len() as u64 }))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawingRequest {
    scene: Value,
    title: Option<String>,
    #[serde(alias = "folder_id")]
    folder_id: Option<Uuid>,
    preview: Option<String>,
}

#[utoipa::path(
    put,
    path = "/v1/bookmarks/{id}/drawing",
    params(
        ("id" = Uuid, Path, description = "Resource ID"),
    ),
    responses(
        (status = 200, description = "OK", body = IdResponse)
    ),
    tag = "bookmarks"
)]
pub async fn save_drawing(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<DrawingRequest>,
) -> Result<Json<crate::modules::responses::IdResponse>, AppError> {
    BookmarksService::new(pool.clone(), None)
        .check_folder(user.id, req.folder_id)
        .await?;

    if !req.scene.is_object() {
        return Err(AppError::BadRequest("Drawing scene must be an object".into()));
    }

    if req
        .preview
        .as_deref()
        .is_some_and(|s| !s.starts_with("data:image/png;base64,") && !s.starts_with("data:image/webp;base64,"))
    {
        return Err(AppError::BadRequest("Invalid drawing preview".into()));
    }

    let mut tx = pool.begin().await?;
    type ExistingRow = (Uuid, String, Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>);
    let existing: Option<ExistingRow> = sqlx::query_as(
        "SELECT user_id, item_type, folder_id, deleted_at FROM bookmarks WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let mut owner = user.id;
    if let Some((saved_owner, kind, folder, deleted)) = existing {
        if kind != "DRAWING" || deleted.is_some() {
            return Err(AppError::NotFound("Drawing not found".into()));
        }
        if saved_owner != user.id {
            let Some(folder) = folder else {
                return Err(AppError::NotFound("Drawing not found".into()));
            };
            if req.folder_id != Some(folder)
                || crate::modules::folders::collaboration::access(&pool, user.id, folder).await? != "EDITOR"
            {
                return Err(AppError::Forbidden("This drawing is read-only".into()));
            }
        }
        owner = saved_owner;
    }

    let saved = sqlx::query(
        r#"
        INSERT INTO bookmarks (id, user_id, item_type, title, folder_id, drawing, thumbnail_url, metadata_status)
        VALUES ($1, $2, 'DRAWING', coalesce($3, 'Untitled drawing'), $4, $5, $6, 'COMPLETED')
        ON CONFLICT (id) DO UPDATE SET
            title = coalesce($3, bookmarks.title),
            folder_id = $4,
            drawing = $5,
            thumbnail_url = coalesce($6, bookmarks.thumbnail_url),
            updated_at = now()
        WHERE bookmarks.user_id = $2
          AND bookmarks.item_type = 'DRAWING'
          AND bookmarks.deleted_at IS NULL
          AND (bookmarks.user_id = $7 OR EXISTS (SELECT 1 FROM folder_members m WHERE m.folder_id = bookmarks.folder_id AND m.user_id = $7 AND m.role = 'EDITOR'))
        "#,
    )
    .bind(id)
    .bind(owner)
    .bind(req.title)
    .bind(req.folder_id)
    .bind(req.scene)
    .bind(req.preview)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;

    if saved.rows_affected() != 1 {
        return Err(AppError::NotFound("Drawing not found".into()));
    }

    tx.commit().await?;
    Ok(Json(IdResponse { id }))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/health",
    responses(
        (status = 200, description = "OK", body = LinkHealthSummary)
    ),
    tag = "bookmarks"
)]
pub async fn health(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<crate::modules::responses::LinkHealthSummary>, AppError> {
    let (total, broken, ok, redirected, favorites, archived, trash, last): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        r#"
        SELECT
            count(*) FILTER (WHERE deleted_at IS NULL),
            count(*) FILTER (WHERE deleted_at IS NULL AND link_health = 'BROKEN'),
            count(*) FILTER (WHERE deleted_at IS NULL AND link_health = 'OK'),
            count(*) FILTER (WHERE deleted_at IS NULL AND link_health = 'REDIRECTED'),
            count(*) FILTER (WHERE deleted_at IS NULL AND is_favorite = true),
            count(*) FILTER (WHERE deleted_at IS NULL AND is_archived = true),
            count(*) FILTER (WHERE deleted_at IS NOT NULL),
            max(last_checked_at)
        FROM bookmarks
        WHERE user_id = $1
        "#,
    )
    .bind(user.id)
    .fetch_one(&pool)
    .await?;

    Ok(Json(LinkHealthSummary {
        total,
        broken,
        ok,
        healthy: ok,
        redirected,
        favorites,
        archived,
        trash,
        last_check: last,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/health/check",
    responses(
        (status = 200, description = "OK", body = TaskQueuedResponse)
    ),
    tag = "bookmarks"
)]
pub async fn check_health(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<crate::modules::responses::TaskQueuedResponse>, AppError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;

    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM background_tasks WHERE user_id = $1 AND kind = 'LINK_CHECK' AND status IN ('PENDING', 'RUNNING'))",
    )
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;

    if active {
        return Err(AppError::Conflict("A link check is already active; check Activity".into()));
    }

    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO background_tasks (id, user_id, kind, status) VALUES ($1, $2, 'LINK_CHECK', 'PENDING')")
        .bind(id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    crate::modules::tasks::service::TasksService::new(pool, None).start_task(id);
    Ok(Json(TaskQueuedResponse { id, status: "PENDING".into() }))
}

#[derive(Deserialize)]
pub struct UrlQuery {
    url: String,
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/scrape",
    params(
        ("url" = String, Query, description = "Page URL"),
    ),
    responses(
        (status = 200, description = "OK", body = ScrapeResponse)
    ),
    tag = "bookmarks"
)]
pub async fn scrape(
    CurrentUser(_user): CurrentUser,
    Query(q): Query<UrlQuery>,
) -> Result<Json<crate::modules::responses::ScrapeResponse>, AppError> {
    let client = crate::infra::http_client::SsrfSafeClient::new()?;
    let m = crate::worker::enrichment::MetadataExtractor::new(client)
        .fetch_and_extract(&q.url)
        .await?;

    Ok(Json(ScrapeResponse {
        title: m.title,
        description: m.description,
        image_url: m.image_url,
        favicon_url: m.favicon_url,
        site_name: m.site_name,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/embeddable",
    params(
        ("url" = String, Query, description = "Page URL"),
    ),
    responses(
        (status = 200, description = "OK", body = EmbeddableResponse)
    ),
    tag = "bookmarks"
)]
pub async fn embeddable(
    CurrentUser(_user): CurrentUser,
    Query(q): Query<UrlQuery>,
) -> Result<Json<crate::modules::responses::EmbeddableResponse>, AppError> {
    let Ok(client) = crate::infra::http_client::SsrfSafeClient::new() else {
        return Ok(Json(EmbeddableResponse { embeddable: false }));
    };
    let Ok(r) = client.get_safe(&q.url).await else {
        return Ok(Json(EmbeddableResponse { embeddable: false }));
    };

    let xfo = r
        .headers()
        .get("x-frame-options")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase());

    let xfo_blocked = xfo.is_some_and(|s| s.contains("deny") || s.contains("sameorigin"));
    let csp_blocked = r
        .headers()
        .get("content-security-policy")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|csp| {
            let Some(dir) = csp
                .split([',', ';'])
                .map(|p| p.trim().to_ascii_lowercase())
                .find(|p| p.starts_with("frame-ancestors"))
            else {
                return false;
            };
            !dir.split_whitespace()
                .skip(1)
                .any(|s| matches!(s, "*" | "https:" | "http:"))
        });

    Ok(Json(EmbeddableResponse { embeddable: !(xfo_blocked || csp_blocked) }))
}

fn proxied_page_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox"),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
    h
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn proxy_error_page(message: &str, reason: Option<&str>) -> (StatusCode, HeaderMap, String) {
    let payload = json!({
        "type": "cabinet-preview-error",
        "message": message,
        "reason": reason
    })
    .to_string()
    .replace('<', "\\u003c");

    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Preview unavailable</title><script>if(window.parent!==window)window.parent.postMessage({payload},\"*\");</script></head><body style=\"font:14px system-ui,sans-serif;color:#555;padding:24px\"><p>{}</p></body></html>",
        escape_html(message)
    );
    (StatusCode::OK, proxied_page_headers(), html)
}

fn strip_script_tags(html: &str) -> String {
    let mut out = html.to_string();
    let mut cursor = 0;
    while cursor < out.len() {
        let lower = out[cursor..].to_ascii_lowercase();
        let Some(rel_start) = lower.find("<script") else {
            break;
        };
        let start = cursor + rel_start;
        let lower_after = out[start..].to_ascii_lowercase();
        if let Some(rel_end) = lower_after.find("</script>") {
            out.replace_range(start..start + rel_end + 9, "");
            cursor = start;
        } else if let Some(rel_gt) = lower_after.find('>') {
            out.replace_range(start..start + rel_gt + 1, "");
            cursor = start;
        } else {
            out.truncate(start);
            break;
        }
    }
    out
}

fn should_strip_page_scripts(url_str: &str, html: &str) -> bool {
    if let Ok(parsed) = url::Url::parse(url_str) {
        if let Some(host) = parsed.host_str() {
            let h = host.to_ascii_lowercase();
            if h == "medium.com"
                || h.ends_with(".medium.com")
                || h == "substack.com"
                || h.ends_with(".substack.com")
            {
                return true;
            }
        }
    }
    html.contains("cdn-client.medium.com") || html.contains("__reactRouterContext")
}

fn strip_csp_and_base_tags(html: &str) -> String {
    let mut out = html.to_string();
    let mut cursor = 0;
    loop {
        let lower = out[cursor..].to_ascii_lowercase();
        let Some(rel) = lower.find("<meta").or_else(|| lower.find("<base")) else {
            break;
        };
        let start = cursor + rel;
        let Some(end_rel) = out[start..].find('>') else {
            break;
        };
        let end = start + end_rel + 1;
        let tag_lower = out[start..end].to_ascii_lowercase();
        if tag_lower.starts_with("<base") || tag_lower.contains("content-security-policy") {
            out.replace_range(start..end, "");
            cursor = start;
        } else {
            cursor = end;
        }
    }
    out
}

fn rewrite_proxied_page(html: &str, page_url: &str, strip_scripts: bool) -> String {
    const STORAGE_SHIM: &str = r##"(function(){try{var s={};var m={getItem:function(k){return s[k]||null;},setItem:function(k,v){s[k]=String(v);},removeItem:function(k){delete s[k];},clear:function(){s={};},key:function(i){return Object.keys(s)[i]||null;},get length(){return Object.keys(s).length;}};Object.defineProperty(window,"sessionStorage",{value:m,configurable:true});Object.defineProperty(window,"localStorage",{value:m,configurable:true});}catch(e){}})();"##;
    const NAV_SCRIPT: &str = r##"(function(){if(window.parent===window)return;document.addEventListener("click",function(event){if(event.defaultPrevented||event.button!==0||event.metaKey||event.ctrlKey||event.shiftKey||event.altKey)return;var link=event.target&&event.target.closest?event.target.closest("a[href]"):null;if(!link||link.target==="_blank"||link.hasAttribute("download"))return;var href=link.href;if(!/^https?:/i.test(href))return;if(href.split("#")[0]===location.href.split("#")[0]&&href.indexOf("#")!==-1)return;event.preventDefault();window.parent.postMessage({type:"cabinet-preview-navigate",url:href},"*");},true);})();"##;
    const READY_SCRIPT: &str = r##"(function(){if(window.parent===window)return;var post=function(type){window.parent.postMessage({type:type},"*");};var ticks=0,peak=0,steady=0,ready=false;var timer=setInterval(function(){ticks++;var body=document.body;var text=body?body.innerText.trim():"";var image=Array.from(document.images).some(function(img){return img.complete&&img.naturalWidth>=200&&img.getBoundingClientRect().height>=100;});peak=Math.max(peak,text.length);var crashed=peak>=40&&(text.length<peak/4||/client-side exception has occurred/i.test(text));if(crashed){clearInterval(timer);post("cabinet-preview-error");}else if(!ready){var settled=document.readyState==="complete"||ticks>=8;steady=(text.length>=40||image)&&settled?steady+1:0;if(steady>=2){ready=true;post("cabinet-preview-ready");}else if(ticks>=40){clearInterval(timer);post("cabinet-preview-error");}}else if(ticks>=80){clearInterval(timer);}},200);})();"##;

    let must_strip = strip_scripts || should_strip_page_scripts(page_url, html);
    let out = if must_strip {
        strip_script_tags(html)
    } else {
        html.to_string()
    };
    let mut out = strip_csp_and_base_tags(&out);
    let injection = format!(
        "<base href=\"{}\" target=\"_self\"><script>{STORAGE_SHIM}</script><script>{NAV_SCRIPT}</script><script>{READY_SCRIPT}</script>",
        escape_html(page_url)
    );
    let lower = out.to_ascii_lowercase();
    if let Some(h) = lower.find("<head") {
        if let Some(close) = lower[h..].find('>') {
            let idx = h + close + 1;
            out.insert_str(idx, &injection);
            return out;
        }
    }
    format!("<head>{injection}</head>{out}")
}

#[derive(Deserialize)]
struct ProxyPageQuery {
    url: String,
    #[allow(dead_code)]
    v: Option<String>,
    /// From POST /v1/bookmarks/proxy-image/sign with `kind: "page"`.
    sig: Option<String>,
}

async fn proxy_page(Query(q): Query<ProxyPageQuery>) -> impl IntoResponse {
    // Only URLs this API signed: stops the preview proxy from being an open, SSRF-capable fetch relay.
    if !q.sig.as_deref().is_some_and(|sig| crate::infra::proxy_sign::verify_page(&q.url, sig)) {
        return (StatusCode::FORBIDDEN, proxied_page_headers(), "Missing or invalid preview signature".to_string());
    }
    let Ok(client) = crate::infra::http_client::SsrfSafeClient::new() else {
        return proxy_error_page("Could not initialize preview client.", None);
    };

    let mut resp = match client.get_safe(&q.url).await {
        Ok(r) => r,
        Err(e) => return proxy_error_page(&format!("This address cannot be previewed: {e}"), None),
    };

    let status = resp.status().as_u16();
    if matches!(status, 401 | 403 | 429)
        || (status == 503
            && (resp.headers().contains_key("cf-mitigated")
                || resp
                    .headers()
                    .get("server")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| s.to_ascii_lowercase().contains("cloudflare"))))
    {
        if let Ok((rendered_html, final_url)) = client.fetch_html_with_browser(&q.url).await {
            return (
                StatusCode::OK,
                proxied_page_headers(),
                rewrite_proxied_page(&rendered_html, &final_url, true),
            );
        }
        return proxy_error_page("This site blocked preview requests.", Some("blocked"));
    }

    if status >= 400 {
        return proxy_error_page("This preview could not be loaded.", None);
    }

    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !ct.is_empty() && !ct.contains("text/html") && !ct.contains("application/xhtml+xml") {
        return proxy_error_page("Only web pages can be previewed here.", None);
    }

    let final_url = resp.url().to_string();
    let max_bytes = 5 * 1024 * 1024;
    let mut buf = Vec::with_capacity(64 * 1024);
    while let Ok(Some(chunk)) = resp.chunk().await {
        if buf.len() + chunk.len() > max_bytes {
            return proxy_error_page("This page is too large to preview.", None);
        }
        buf.extend_from_slice(&chunk);
    }

    let html = String::from_utf8_lossy(&buf);
    (
        StatusCode::OK,
        proxied_page_headers(),
        rewrite_proxied_page(&html, &final_url, false),
    )
}
