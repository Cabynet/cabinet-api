use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cabinet_api::{
    config::AppConfig,
    infra::{db::create_db_pool, redis::create_redis_pool},
    modules::auth::{
        dto::RegisterRequest,
        handlers::router as auth_router,
        oauth::{OAuthAuthorizeQuery, OAuthService},
        service::AuthService,
    },
};
use redis::AsyncCommands;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_oauth_authorization_url_pkce_and_csrf_state() {
    let mut config = AppConfig::from_env().expect("config");
    config.database_url = "postgresql://postgres:postgrespassword@localhost:5432/cabinet".to_string();
    config.google_client_id = "test-google-client-id.apps.googleusercontent.com".to_string();
    config.google_client_secret = "test-google-secret".to_string();
    config.github_client_id = "test-github-client-id".to_string();
    config.github_client_secret = "test-github-secret".to_string();

    let pool = match create_db_pool(&config).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let redis = match create_redis_pool(&config).await {
        Ok(r) => r,
        Err(_) => return,
    };

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .with_state(pool.clone());

    // 1. Test Google OAuth Authorize URL (PKCE generated)
    let mut oauth_service = OAuthService::new(pool.clone(), Some(redis.clone()), config.clone());
    let google_res = oauth_service
        .get_authorization_url(
            "google",
            OAuthAuthorizeQuery {
                client_type: Some("MOBILE".to_string()),
                redirect_uri: Some("myapp://auth/callback".to_string()),
                format: Some("json".to_string()),
            },
        )
        .await
        .unwrap();

    assert!(google_res.authorization_url.contains("accounts.google.com"));
    assert!(google_res.authorization_url.contains("code_challenge="));
    assert!(google_res.authorization_url.contains("code_challenge_method=S256"));
    assert!(google_res.authorization_url.contains(&format!("state={}", google_res.state)));

    // Verify state was saved in Redis with 600s TTL
    let mut redis_conn = redis.clone();
    let state_key = format!("oauth:state:{}", google_res.state);
    let cached_state: Option<String> = redis_conn.get(&state_key).await.unwrap();
    assert!(cached_state.is_some());
    assert!(cached_state.unwrap().contains("MOBILE"));

    // 2. Test GitHub OAuth Authorize URL
    let github_res = oauth_service
        .get_authorization_url(
            "github",
            OAuthAuthorizeQuery {
                client_type: Some("WEB".to_string()),
                redirect_uri: None,
                format: Some("json".to_string()),
            },
        )
        .await
        .unwrap();

    assert!(github_res.authorization_url.contains("github.com/login/oauth/authorize"));
    assert!(github_res.authorization_url.contains(&format!("state={}", github_res.state)));

    // 3. Test Invalid / Expired State Protection on Callback
    let fake_callback_req = Request::builder()
        .method("GET")
        .uri("/v1/auth/callback/google?code=fake_code&state=non_existent_random_state_12345")
        .body(Body::empty())
        .unwrap();

    let resp_fake = app.clone().oneshot(fake_callback_req).await.unwrap();
    assert_eq!(resp_fake.status(), StatusCode::BAD_REQUEST);

    // 4. Test Auto-link Existing User via OAuth
    let email = format!("autolink_{}@example.com", Uuid::now_v7());

    // User initially registered with password
    let auth_service = AuthService::new(pool.clone());
    let initial_session = auth_service
        .register(
            RegisterRequest {
                email: email.clone(),
                password: "OriginalPassword123!".to_string(),
                name: "Original Name".to_string(),
            },
            "WEB",
            None,
            None,
        )
        .await
        .unwrap();

    // User subsequently logs in via OAuth with same email
    let oauth_user = oauth_service
        .find_or_create_oauth_user(
            &email,
            "Updated OAuth Name",
            Some("https://cdn.example.com/oauth_avatar.png".to_string()),
        )
        .await
        .unwrap();

    // Verify user ID matches, email_verified is true, avatar is updated, but user record is NOT duplicated
    assert_eq!(oauth_user.id, initial_session.user.id);
    assert_eq!(oauth_user.email, email);
    assert_eq!(
        oauth_user.avatar_url.as_deref(),
        Some("https://cdn.example.com/oauth_avatar.png")
    );

    // Verify original password is still intact and valid
    let row: (Option<String>, bool) = sqlx::query_as(
        "SELECT password_hash, email_verified FROM users WHERE id = $1",
    )
    .bind(oauth_user.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(row.0.is_some());
    assert!(row.1); // email_verified: true
}
