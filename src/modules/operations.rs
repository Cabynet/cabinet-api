use axum::{extract::State, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::Instant,
};
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteMetrics {
    pub name: String,
    pub count: u64,
    pub errors: u64,
    pub samples: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationsMetrics {
    pub uptime_seconds: u64,
    pub rss_bytes: Option<u64>,
    pub routes: Vec<RouteMetrics>,
    pub tasks: BTreeMap<String, i64>,
    pub metadata: BTreeMap<String, i64>,
    pub sampling: String,
}

type RouteStats = (u64, u64, Vec<f64>);
type RouteMap = BTreeMap<String, RouteStats>;

static START: OnceLock<Instant> = OnceLock::new();
static ROUTES: OnceLock<Mutex<RouteMap>> = OnceLock::new();

pub fn init() {
    START.get_or_init(Instant::now);
}

pub fn record(route: String, status: u16, ms: f64) {
    let Ok(mut routes) = ROUTES.get_or_init(|| Mutex::new(BTreeMap::new())).lock() else {
        return;
    };
    let key = if routes.len() >= 100 && !routes.contains_key(&route) {
        "overflow".into()
    } else {
        route
    };
    let item = routes.entry(key).or_default();
    item.0 += 1;
    if status >= 500 {
        item.1 += 1;
    }
    if item.2.len() == 256 {
        item.2.remove(0);
    }
    item.2.push(ms);
}

fn is_operator(id: Uuid) -> bool {
    std::env::var("OPERATIONS_USER_IDS")
        .unwrap_or_default()
        .split(',')
        .any(|s| s.trim() == id.to_string())
}

pub fn router() -> Router<PgPool> {
    router_routes().layer(axum::middleware::from_fn(crate::infra::camel::camelize_json))
}

fn router_routes() -> Router<PgPool> {
    Router::new()
        .route("/access", get(access))
        .route("/metrics", get(metrics))
}

#[utoipa::path(
    get,
    path = "/v1/operations/access",
    responses(
        (status = 200, description = "OK", body = OperationsAccessResponse)
    ),
    tag = "operations"
)]
pub async fn access(CurrentUser(user): CurrentUser) -> Json<crate::modules::responses::OperationsAccessResponse> {
    Json(crate::modules::responses::OperationsAccessResponse { enabled: is_operator(user.id) })
}

#[utoipa::path(
    get,
    path = "/v1/operations/metrics",
    responses(
        (status = 200, description = "OK", body = OperationsMetrics)
    ),
    tag = "operations"
)]
pub async fn metrics(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
) -> Result<([(&'static str, &'static str); 1], Json<OperationsMetrics>), AppError> {
    if !is_operator(user.id) {
        return Err(AppError::Forbidden("Operator access required".into()));
    }

    let tasks: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM background_tasks GROUP BY status")
            .fetch_all(&pool)
            .await?;

    let metadata: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT metadata_status, count(*)
        FROM bookmarks
        WHERE deleted_at IS NULL AND item_type = 'LINK'
        GROUP BY metadata_status
        "#,
    )
    .fetch_all(&pool)
    .await?;

    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024);

    let routes = ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map(|r| {
            r.iter()
                .map(|(name, (count, errors, samples))| {
                    let mut sorted = samples.clone();
                    sorted.sort_by(f64::total_cmp);
                    RouteMetrics {
                        name: name.clone(),
                        count: *count,
                        errors: *errors,
                        samples: sorted.len(),
                        p50_ms: sorted.get(sorted.len().saturating_sub(1) / 2).copied(),
                        p95_ms: sorted.get(sorted.len().saturating_sub(1) * 95 / 100).copied(),
                        max_ms: sorted.last().copied(),
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok((
        [("Cache-Control", "private, no-store")],
        Json(OperationsMetrics {
            uptime_seconds: START.get_or_init(Instant::now).elapsed().as_secs(),
            rss_bytes: rss,
            routes,
            tasks: tasks.into_iter().collect(),
            metadata: metadata.into_iter().collect(),
            sampling: "Process-local; last 256 latency samples per route. RSS is available on Linux only.".into(),
        }),
    ))
}
