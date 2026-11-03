use std::env;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub redis_url: String,
    pub frontend_url: String,
    pub cookie_domain: Option<String>,
    pub max_db_connections: u32,
    pub api_url: String,

    // OAuth
    pub google_client_id: String,
    pub google_client_secret: String,
    pub github_client_id: String,
    pub github_client_secret: String,

    // Cloudflare R2
    pub r2_endpoint: String,
    pub r2_account_id: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
    pub r2_bucket_name: String,
    pub r2_public_url: String,

    // Worker configuration
    pub interactive_worker_concurrency: usize,
    pub bulk_worker_concurrency: usize,
}

impl AppConfig {
    /// Parsed once per process; handlers use this instead of re-reading the environment per request.
    pub fn shared() -> Result<&'static AppConfig, anyhow::Error> {
        static CONFIG: std::sync::OnceLock<AppConfig> = std::sync::OnceLock::new();
        if let Some(config) = CONFIG.get() {
            return Ok(config);
        }
        let config = Self::from_env()?;
        Ok(CONFIG.get_or_init(|| config))
    }

    pub fn from_env() -> Result<Self, anyhow::Error> {
        dotenvy::dotenv().ok();

        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("PORT")
            .unwrap_or_else(|_| "4000".to_string())
            .parse::<u16>()?;

        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://postgres:postgrespassword@localhost:5432/cabinet?schema=public".to_string());

        let redis_url = env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let frontend_url = env::var("FRONTEND_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        // Browsers reject Domain=localhost, so treat it as "host-only cookie" (the dev default).
        let cookie_domain = env::var("COOKIE_DOMAIN").ok().filter(|s| !s.is_empty() && s != "localhost");
        let api_url = env::var("API_URL").unwrap_or_else(|_| "http://localhost:4000".to_string());

        let google_client_id = env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
        let google_client_secret = env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default();
        let github_client_id = env::var("GITHUB_CLIENT_ID").unwrap_or_default();
        let github_client_secret = env::var("GITHUB_CLIENT_SECRET").unwrap_or_default();

        // Tuned for VPS 2GB RAM: Keep pool connections conservative
        let max_db_connections = env::var("MAX_DB_CONNECTIONS")
            .unwrap_or_else(|_| "15".to_string())
            .parse::<u32>()?;

        let r2_account_id = env::var("R2_ACCOUNT_ID").unwrap_or_default();
        let r2_endpoint = env::var("R2_ENDPOINT").unwrap_or_else(|_|format!("https://{}.r2.cloudflarestorage.com",r2_account_id));
        let r2_access_key_id = env::var("R2_ACCESS_KEY_ID").unwrap_or_default();
        let r2_secret_access_key = env::var("R2_SECRET_ACCESS_KEY").unwrap_or_default();
        let r2_bucket_name = env::var("R2_BUCKET_NAME").unwrap_or_else(|_| "cabinet".to_string());
        let r2_public_url = env::var("R2_PUBLIC_URL").unwrap_or_default();

        let interactive_worker_concurrency = env::var("INTERACTIVE_WORKER_CONCURRENCY")
            .unwrap_or_else(|_| "4".to_string())
            .parse::<usize>()?;

        let bulk_worker_concurrency = env::var("BULK_WORKER_CONCURRENCY")
            .unwrap_or_else(|_| "2".to_string())
            .parse::<usize>()?;

        Ok(Self {
            host,
            port,
            database_url,
            redis_url,
            frontend_url,
            cookie_domain,
            max_db_connections,
            api_url,
            google_client_id,
            google_client_secret,
            github_client_id,
            github_client_secret,
            r2_endpoint,
            r2_account_id,
            r2_access_key_id,
            r2_secret_access_key,
            r2_bucket_name,
            r2_public_url,
            interactive_worker_concurrency,
            bulk_worker_concurrency,
        })
    }
}
