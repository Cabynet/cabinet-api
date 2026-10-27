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

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
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

    // 3. Connect to Database & Redis
    tracing::info!("Connecting to PostgreSQL: {}", config.database_url);
    let pool = match create_db_pool(&config).await {
        Ok(p) => {
            tracing::info!("PostgreSQL connection established successfully");
            if let Err(e) = sqlx::migrate!("./migrations").run(&p).await {
                tracing::warn!("Migration error: {}. Continuing...", e);
            } else {
                tracing::info!("Database migrations applied successfully");
            }
            p
        }
        Err(e) => {
            tracing::warn!("Could not connect to PostgreSQL immediately: {}. Proceeding in offline/development mode.", e);
            // In dev without running PG, allow fallback or retry
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_millis(500))
                .connect_lazy(&config.database_url)
            {
                Ok(lazy_p) => lazy_p,
                Err(_) => return Err(e.into()),
            }
        }
    };

    let redis_res = create_redis_pool(&config).await;
    match redis_res {
        Ok(_) => tracing::info!("Redis connection established successfully"),
        Err(ref e) => tracing::warn!("Redis connection skipped or offline: {}", e),
    }

    // 4. Configure CORS for Web, Extension, Tauri, and Mobile
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            let s = origin.to_str().unwrap_or_default();
            s.starts_with("http://localhost")
                || s.starts_with("https://localhost")
                || s.starts_with("chrome-extension://")
                || s.starts_with("moz-extension://")
                || s.starts_with("tauri://")
                || s.ends_with(".cabinety.space")
                || s.ends_with(".vercel.app")
                || s.ends_with(".pages.dev")
        }))
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
    let api_routes = Router::new()
        .nest("/auth", modules::auth::handlers::router())
        .nest("/bookmarks", modules::bookmarks::handlers::router())
        .nest("/folders", modules::folders::handlers::router())
        .nest("/tags", modules::tags::handlers::router())
        .nest("/notes", modules::notes::handlers::router())
        .nest("/media", modules::media::handlers::router())
        .nest("/search", modules::search::handlers::router())
        .nest("/tasks", modules::tasks::handlers::router())
        .with_state(pool.clone());

    let app = Router::new()
        .route("/v1/health", get(|| async {
            Json(serde_json::json!({
                "status": "healthy",
                "service": "cabinet-api",
                "version": env!("CARGO_PKG_VERSION")
            }))
        }))
        .nest("/v1", api_routes)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
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
