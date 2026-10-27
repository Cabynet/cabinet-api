use cabinet_api::{
    config::AppConfig,
    infra::{
        db::create_db_pool,
        http_client::SsrfSafeClient,
        redis::create_redis_pool,
    },
    modules::bookmarks::normalizer::normalize_url,
    worker::{
        enrichment::MetadataExtractor,
        queue::{EnrichmentJob, WorkerProcessor, QUEUE_BULK, QUEUE_INTERACTIVE},
    },
};
use redis::AsyncCommands;
use std::net::IpAddr;
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn test_ssrf_guard_blocks_private_and_metadata_ips() {
    let client = SsrfSafeClient::new().expect("Client builder");

    // 1. IP checks
    let loopback_v4: IpAddr = "127.0.0.1".parse().unwrap();
    let loopback_v6: IpAddr = "::1".parse().unwrap();
    let cloud_meta: IpAddr = "169.254.169.254".parse().unwrap();
    let priv_10: IpAddr = "10.1.2.3".parse().unwrap();
    let priv_192: IpAddr = "192.168.1.1".parse().unwrap();
    let priv_172: IpAddr = "172.20.0.5".parse().unwrap();
    let public_dns: IpAddr = "1.1.1.1".parse().unwrap();

    assert!(!SsrfSafeClient::is_ip_safe(loopback_v4));
    assert!(!SsrfSafeClient::is_ip_safe(loopback_v6));
    assert!(!SsrfSafeClient::is_ip_safe(cloud_meta));
    assert!(!SsrfSafeClient::is_ip_safe(priv_10));
    assert!(!SsrfSafeClient::is_ip_safe(priv_192));
    assert!(!SsrfSafeClient::is_ip_safe(priv_172));
    assert!(SsrfSafeClient::is_ip_safe(public_dns));

    // 2. URL safety checks
    let bad_local = Url::parse("http://127.0.0.1:4000/v1/health").unwrap();
    assert!(client.validate_url_safety(&bad_local).await.is_err());

    let bad_meta = Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
    assert!(client.validate_url_safety(&bad_meta).await.is_err());

    let bad_scheme = Url::parse("file:///etc/passwd").unwrap();
    assert!(client.validate_url_safety(&bad_scheme).await.is_err());
}

#[test]
fn test_streaming_html_parser_extracts_metadata_correctly() {
    let sample_html = r#"
        <!DOCTYPE html>
        <html lang="en">
        <head>
            <meta charset="utf-8">
            <title>Ignored HTML Title Tag</title>
            <meta property="og:title" content="Real OG Title">
            <meta property="og:description" content="This is an accurate OpenGraph description.">
            <meta property="og:image" content="/images/preview.png">
            <meta property="og:site_name" content="Rust Foundation">
            <link rel="icon" href="/static/favicon.png">
        </head>
        <body>
            <h1>Giant content body that should be ignored</h1>
        </body>
        </html>
    "#;

    let meta = MetadataExtractor::parse_html(sample_html.as_bytes(), "https://example.org/path/article")
        .expect("Parsed metadata");

    assert_eq!(meta.title.as_deref(), Some("Real OG Title"));
    assert_eq!(meta.description.as_deref(), Some("This is an accurate OpenGraph description."));
    assert_eq!(meta.image_url.as_deref(), Some("https://example.org/images/preview.png"));
    assert_eq!(meta.favicon_url.as_deref(), Some("https://example.org/static/favicon.png"));
    assert_eq!(meta.site_name.as_deref(), Some("Rust Foundation"));
}

#[tokio::test]
async fn test_worker_dual_queue_and_fill_missing_logic() {
    let mut config = AppConfig::from_env().expect("config");
    config.database_url = "postgresql://postgres:postgrespassword@localhost:5432/cabinet".to_string();

    let pool = match create_db_pool(&config).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let redis = match create_redis_pool(&config).await {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut processor = WorkerProcessor::new(pool.clone(), redis.clone()).unwrap();

    // 1. Create a user
    let user_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, email, name) VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(format!("worker_{}@example.com", user_id))
    .bind("Worker User")
    .execute(&pool)
    .await
    .unwrap();

    // 2. Create Bookmark with user_edited_title = TRUE and description = NULL
    let bookmark_id = Uuid::now_v7();
    let norm = normalize_url("https://example.com/test-article").unwrap();

    sqlx::query(
        r#"
        INSERT INTO bookmarks (
            id, user_id, raw_url, normalized_url, url_hash, normalized_domain,
            title, user_edited_title, description, user_edited_desc, metadata_status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, TRUE, NULL, FALSE, 'PENDING')
        "#,
    )
    .bind(bookmark_id)
    .bind(user_id)
    .bind(&norm.raw_url)
    .bind(&norm.normalized_url)
    .bind(&norm.url_hash)
    .bind(&norm.domain)
    .bind("User Preserved Title")
    .execute(&pool)
    .await
    .unwrap();

    // 3. Pre-seed Redis cache so worker doesn't need to make an external network request in test
    let hash_hex = hex::encode(&norm.url_hash);
    let cache_key = format!("meta:cache:{}", hash_hex);
    let mock_meta = cabinet_api::worker::enrichment::ExtractedMetadata {
        title: Some("Remote Scraped Title".to_string()),
        description: Some("Remote Enriched Description".to_string()),
        image_url: Some("https://example.com/thumb.webp".to_string()),
        favicon_url: Some("https://example.com/favicon.ico".to_string()),
        site_name: Some("Example Site".to_string()),
    };
    let json_str = serde_json::to_string(&mock_meta).unwrap();
    let mut redis_conn = redis.clone();
    let _: Result<(), redis::RedisError> = redis_conn.set_ex(&cache_key, json_str, 300).await;

    // 4. Test Dual-Queue Priority
    // Enqueue Job A to QUEUE_BULK and Job B to QUEUE_INTERACTIVE
    let job_bulk = EnrichmentJob {
        bookmark_id: Uuid::now_v7(),
        user_id,
        url: "https://bulk.example.com".to_string(),
        url_hash: vec![1, 2, 3],
        attempt: 0,
    };
    let job_interactive = EnrichmentJob {
        bookmark_id,
        user_id,
        url: norm.raw_url.clone(),
        url_hash: norm.url_hash.clone(),
        attempt: 0,
    };

    let _: Result<(), redis::RedisError> = redis_conn
        .lpush(QUEUE_BULK, serde_json::to_string(&job_bulk).unwrap())
        .await;
    let _: Result<(), redis::RedisError> = redis_conn
        .lpush(QUEUE_INTERACTIVE, serde_json::to_string(&job_interactive).unwrap())
        .await;

    // First poll MUST pop from QUEUE_INTERACTIVE (Priority 1)!
    let processed = processor.poll_and_process_one().await;
    assert!(processed);

    // 5. Verify database "Fill-Missing, Never Overwrite User Intent":
    // - Title was user_edited_title = TRUE -> MUST remain "User Preserved Title"
    // - Description was user_edited_desc = FALSE -> MUST be filled with "Remote Enriched Description"
    // - Status must be 'COMPLETED'
    let row: (String, Option<String>, Option<String>, String) = sqlx::query_as(
        "SELECT title, description, thumbnail_url, metadata_status FROM bookmarks WHERE id = $1",
    )
    .bind(bookmark_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.0, "User Preserved Title"); // Preserved!
    assert_eq!(row.1.as_deref(), Some("Remote Enriched Description")); // Filled!
    assert_eq!(row.2.as_deref(), Some("https://example.com/thumb.webp")); // Enriched!
    assert_eq!(row.3, "COMPLETED");
}
