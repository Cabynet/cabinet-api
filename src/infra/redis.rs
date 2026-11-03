use redis::aio::ConnectionManager;
use redis::Client;
use crate::config::AppConfig;

pub async fn create_redis_pool(config: &AppConfig) -> Result<ConnectionManager, redis::RedisError> {
    let client = Client::open(config.redis_url.clone())?;
    client.get_connection_manager().await
}

/// One shared connection manager (cheap to clone). Retries on each call until Redis is reachable.
pub async fn shared() -> Option<ConnectionManager> {
    static REDIS: tokio::sync::OnceCell<ConnectionManager> = tokio::sync::OnceCell::const_new();
    let config = AppConfig::shared().ok()?;
    REDIS.get_or_try_init(|| create_redis_pool(config)).await.ok().cloned()
}
