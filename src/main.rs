use std::net::SocketAddr;
use std::time::Duration;
use axum::{
    http::{header, Method},
    response::Json,
    routing::get,
    Router,
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use cabinet_api::config::AppConfig;
use cabinet_api::infra::db::create_db_pool;
use cabinet_api::infra::redis::create_redis_pool;
use cabinet_api::modules;
use cabinet_api::openapi::{generate_openapi_json, ApiDoc};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // 0. Handle CLI flags like --export-openapi
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--export-openapi") {
        let json = generate_openapi_json();
        std::fs::write("openapi.json", &json)?;
        println!("OpenAPI spec exported successfully to openapi.json ({} bytes)", json.len());
        return Ok(());
    }
    // 1. Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cabinet_api=debug,tower_http=debug,axum=trace".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("Starting Cabinet Backend (Modular Monolith - Rust + Axum)");

    // 2. Load configuration
    let config = AppConfig::from_env()?;
    cabinet_api::infra::proxy_sign::init()?;

    // 3. Connect to Database & Redis
    tracing::info!("Connecting to PostgreSQL");
    let pool = create_db_pool(&config).await?;
    let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    sqlx::migrate::Migrator::new(migrations).await?.run(&pool).await?;
    tracing::info!("PostgreSQL connected and migrations applied");

    let redis_res = create_redis_pool(&config).await;
    match redis_res {
        Ok(ref conn) => {
            tracing::info!("Redis connection established successfully");
            let _ = conn;
        }
        Err(ref e) => tracing::warn!("Redis connection skipped or offline: {}", e),
    }

    cabinet_api::worker::spawn_worker_loop(pool.clone(), redis_res.as_ref().ok().cloned());
    modules::tasks::service::spawn_recovery(pool.clone());

    // 4. Configure CORS for Web, Extension, Tauri, and Mobile
    let mut origins = vec![config.frontend_url.trim_end_matches('/').to_owned()];
    origins.extend(std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default().split(',').map(str::trim).filter(|s|!s.is_empty()).map(str::to_owned));
    let cors_origins = origins.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| cors_origins.iter().any(|allowed|origin.as_bytes()==allowed.as_bytes())))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::COOKIE,
            header::HeaderName::from_static("x-client-request-id"),
        ])
        .allow_credentials(true)
        .max_age(Duration::from_secs(3600));

    // 5. Mount API Routes (/v1)
    modules::operations::init();
    let api_routes = Router::new()
        .nest("/activity", modules::activity::router())
        .nest("/operations", modules::operations::router())
        .nest("/auth", modules::auth::handlers::router())
        .nest("/bookmarks", modules::bookmarks::handlers::router())
        .nest("/folders", modules::folders::handlers::router())
        .nest("/tags", modules::tags::handlers::router())
        .nest("/notes", modules::notes::handlers::router())
        .nest("/media", modules::media::handlers::router())
        .nest("/uploads", modules::media::handlers::uploads_router())
        .nest("/search", modules::search::handlers::router())
        .nest("/tasks", modules::tasks::handlers::router())
        .nest("/users", modules::users::handlers::router())
        .nest("/share", modules::folders::handlers::public_share_router())
        .with_state(pool.clone());

    let app_routes = Router::new()
        .merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", ApiDoc::openapi()))
        .route("/v1/health", get(|| async {
            Json(cabinet_api::modules::responses::ServiceHealthResponse {
                status: "healthy".into(),
                service: "cabinet-api".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            })
        }))
        .nest("/v1", api_routes);

    let app = if let Ok(redis_conn) = redis_res {
        app_routes.layer(axum::middleware::from_fn(move |req, next| {
            cabinet_api::infra::rate_limit::rate_limit_middleware(
                redis_conn.clone(),
                cabinet_api::infra::rate_limit::RateLimitConfig::GENERAL,
                req,
                next,
            )
        }))
    } else {
        tracing::error!("Redis unavailable at boot: rate limiting is DISABLED until the API restarts");
        app_routes
    };

    let app = app.layer(axum::middleware::from_fn(move |req: axum::extract::Request, next: axum::middleware::Next| {
        let origins = origins.clone();
        async move {
            if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
                if let Some(origin) = req.headers().get(header::ORIGIN) {
                    if !origins.iter().any(|allowed|origin.as_bytes()==allowed.as_bytes()) {
                        return axum::response::IntoResponse::into_response(cabinet_api::errors::AppError::Forbidden("Origin is not allowed".into()));
                    }
                }
            }
            let route = req.extensions().get::<axum::extract::MatchedPath>().map(|p|format!("{} {}",req.method(),p.as_str())).unwrap_or_else(||"unmatched".into());
            let start = std::time::Instant::now();
            let response = next.run(req).await;
            modules::operations::record(route, response.status().as_u16(), start.elapsed().as_secs_f64()*1000.0);
            response
        }
    }))
    .layer(cors)
    .layer(TraceLayer::new_for_http())
    .layer(axum::extract::DefaultBodyLimit::max(60 * 1024 * 1024));

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Signal received, shutting down gracefully");
}
