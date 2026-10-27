use redis::aio::ConnectionManager;
use redis::Client;
use crate::config::AppConfig;

pub async fn create_redis_pool(config: &AppConfig) -> Result<ConnectionManager, redis::RedisError> {
    let client = Client::open(config.redis_url.clone())?;
    client.get_connection_manager().await
}
