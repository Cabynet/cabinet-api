use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
mod common;
use cabinet_api::{
    infra::db::create_db_pool,
    modules::auth::{
        dto::{MeResponse, RegisterRequest, SessionResponse},
        handlers::router as auth_router,
    },
};
use tower::ServiceExt;

#[tokio::test]
async fn test_dual_auth_cookie_and_bearer() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .with_state(pool);

    let email = format!("test_{}@example.com", uuid::Uuid::now_v7());
    let register_payload = RegisterRequest {
        email: email.clone(),
        password: "SuperSecretPassword123!".to_string(),
        name: "Dual Auth User".to_string(),
    };

    // 1. Test Register
    let req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&register_payload).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let cookie_header = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie header should be present")
        .to_str()
        .unwrap();
    assert!(cookie_header.contains("cabinet_session="));

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
        .await
        .unwrap();
    let session_res: SessionResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(session_res.user.email, email);
    let token = session_res.token;

    // 2. Test /me via Bearer Token (Extension / Mobile / Desktop style)
    let req_bearer = Request::builder()
        .method("GET")
        .uri("/v1/auth/me")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp_bearer = app.clone().oneshot(req_bearer).await.unwrap();
    assert_eq!(resp_bearer.status(), StatusCode::OK);
    let bearer_bytes = axum::body::to_bytes(resp_bearer.into_body(), 1024 * 64)
        .await
        .unwrap();
    let me_bearer: MeResponse = serde_json::from_slice(&bearer_bytes).unwrap();
    assert_eq!(me_bearer.email, email);
    assert_eq!(me_bearer.session.user_id, session_res.user.id);

    // 3. Test /me via Cookie (Web App style)
    let req_cookie = Request::builder()
        .method("GET")
        .uri("/v1/auth/me")
        .header(header::COOKIE, format!("cabinet_session={}", token))
        .body(Body::empty())
        .unwrap();

    let resp_cookie = app.clone().oneshot(req_cookie).await.unwrap();
    assert_eq!(resp_cookie.status(), StatusCode::OK);
    let cookie_bytes = axum::body::to_bytes(resp_cookie.into_body(), 1024 * 64)
        .await
        .unwrap();
    let me_cookie: MeResponse = serde_json::from_slice(&cookie_bytes).unwrap();
    assert_eq!(me_cookie.email, email);
    assert_eq!(me_cookie.session.user_id, session_res.user.id);

    // 4. Test /me without token (should be 401 Unauthorized)
    let req_unauth = Request::builder()
        .method("GET")
        .uri("/v1/auth/me")
        .body(Body::empty())
        .unwrap();

    let resp_unauth = app.clone().oneshot(req_unauth).await.unwrap();
    assert_eq!(resp_unauth.status(), StatusCode::UNAUTHORIZED);

    // 5. Test Logout (revokes session)
    let req_logout = Request::builder()
        .method("POST")
        .uri("/v1/auth/logout")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp_logout = app.clone().oneshot(req_logout).await.unwrap();
    assert_eq!(resp_logout.status(), StatusCode::OK);

    // 6. Test /me after logout with revoked token (should be 401 Unauthorized)
    let req_after_logout = Request::builder()
        .method("GET")
        .uri("/v1/auth/me")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp_after_logout = app.clone().oneshot(req_after_logout).await.unwrap();
    assert_eq!(resp_after_logout.status(), StatusCode::UNAUTHORIZED);
}
