use cabinet_api::config::AppConfig;

pub fn config() -> AppConfig {
    let database = std::env::var("TEST_DATABASE_URL").expect("Set TEST_DATABASE_URL to an isolated *_test database; use scripts/test.sh");
    let redis = std::env::var("TEST_REDIS_URL").expect("Set TEST_REDIS_URL to an isolated Redis instance; use scripts/test.sh");
    assert_eq!(std::env::var("DATABASE_URL").ok().as_deref(), Some(database.as_str()), "Handlers and tests must use the same isolated database");
    assert_eq!(std::env::var("REDIS_URL").ok().as_deref(), Some(redis.as_str()), "Handlers and tests must use the same isolated Redis");
    let url = url::Url::parse(&database).expect("Test database URL");
    assert!(url.path().contains("_test"), "Refusing a database without _test in its name");
    AppConfig::from_env().expect("Test configuration")
}
