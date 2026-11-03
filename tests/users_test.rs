use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
mod common;
use cabinet_api::{
    infra::db::create_db_pool,
    modules::{
        auth::{
            dto::{AuthUser, LoginRequest, RegisterRequest, SessionResponse},
            handlers::router as auth_router,
        },
        users::{
            dto::{ChangePasswordRequest, UpdateUserRequest, UserActionResponse},
            handlers::router as users_router,
        },
    },
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn test_user_profile_change_password_and_delete() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");

    let app = axum::Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/users", users_router())
        .with_state(pool.clone());

    // 1. Register User
    let email = format!("user_mgmt_{}@example.com", Uuid::now_v7());
    let reg_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&RegisterRequest {
                email: email.clone(),
                password: "OldPassword123!".to_string(),
                name: "Initial Name".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let reg_resp = app.clone().oneshot(reg_req).await.unwrap();
    assert_eq!(reg_resp.status(), StatusCode::CREATED);
    let reg_bytes = axum::body::to_bytes(reg_resp.into_body(), 1024 * 64).await.unwrap();
    let session: SessionResponse = serde_json::from_slice(&reg_bytes).unwrap();
    let auth_header = format!("Bearer {}", session.token);

    // 2. Update Profile (Name & Avatar)
    let patch_req = Request::builder()
        .method("PATCH")
        .uri("/v1/users/me")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&UpdateUserRequest {
                name: Some("Updated Display Name".to_string()),
                avatar_url: Some("https://cdn.example.com/new_avatar.png".to_string()),
            })
            .unwrap(),
        ))
        .unwrap();

    let patch_resp = app.clone().oneshot(patch_req).await.unwrap();
    assert_eq!(patch_resp.status(), StatusCode::OK);
    let patch_bytes = axum::body::to_bytes(patch_resp.into_body(), 1024 * 64).await.unwrap();
    let updated_user: AuthUser = serde_json::from_slice(&patch_bytes).unwrap();

    assert_eq!(updated_user.name, "Updated Display Name");
    assert_eq!(updated_user.avatar_url.as_deref(), Some("https://cdn.example.com/new_avatar.png"));

    // 3. Change Password with wrong current password -> 401
    let wrong_pw_req = Request::builder()
        .method("POST")
        .uri("/v1/users/change-password")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&ChangePasswordRequest {
                current_password: Some("WrongCurrentPassword!".to_string()),
                new_password: "NewPassword456!".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let wrong_pw_resp = app.clone().oneshot(wrong_pw_req).await.unwrap();
    assert_eq!(wrong_pw_resp.status(), StatusCode::UNAUTHORIZED);

    // A second session (another device) that a password change must revoke.
    let other_login = Request::builder()
        .method("POST")
        .uri("/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&LoginRequest { email: email.clone(), password: "OldPassword123!".to_string() }).unwrap(),
        ))
        .unwrap();
    let other_resp = app.clone().oneshot(other_login).await.unwrap();
    assert_eq!(other_resp.status(), StatusCode::OK);
    let other_session: SessionResponse =
        serde_json::from_slice(&axum::body::to_bytes(other_resp.into_body(), 1024 * 64).await.unwrap()).unwrap();
    let other_auth = format!("Bearer {}", other_session.token);

    // 4. Change Password with correct current password -> 200
    let right_pw_req = Request::builder()
        .method("POST")
        .uri("/v1/users/change-password")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&ChangePasswordRequest {
                current_password: Some("OldPassword123!".to_string()),
                new_password: "NewPassword456!".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let right_pw_resp = app.clone().oneshot(right_pw_req).await.unwrap();
    assert_eq!(right_pw_resp.status(), StatusCode::OK);
    let right_pw_bytes = axum::body::to_bytes(right_pw_resp.into_body(), 1024 * 64).await.unwrap();
    let action_res: UserActionResponse = serde_json::from_slice(&right_pw_bytes).unwrap();
    assert!(action_res.success);

    // The other session is revoked; the one that changed the password survives.
    let me_status = |token: String| {
        let app = app.clone();
        async move {
            let req = Request::builder().uri("/v1/auth/me").header(header::AUTHORIZATION, token).body(Body::empty()).unwrap();
            app.oneshot(req).await.unwrap().status()
        }
    };
    assert_eq!(me_status(other_auth).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(auth_header.clone()).await, StatusCode::OK);

    // 5. Login with New Password
    let login_req = Request::builder()
        .method("POST")
        .uri("/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&LoginRequest {
                email: email.clone(),
                password: "NewPassword456!".to_string(),
            })
            .unwrap(),
        ))
        .unwrap();

    let login_resp = app.clone().oneshot(login_req).await.unwrap();
    assert_eq!(login_resp.status(), StatusCode::OK);

    // 6. Delete Account requires the current password
    for body in [Body::empty(), Body::from(r#"{"currentPassword":"wrong-password"}"#)] {
        let bad = Request::builder()
            .method("DELETE")
            .uri("/v1/users/me")
            .header(header::AUTHORIZATION, &auth_header)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap();
        assert_eq!(app.clone().oneshot(bad).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }
    let del_req = Request::builder()
        .method("DELETE")
        .uri("/v1/users/me")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"currentPassword":"NewPassword456!"}"#))
        .unwrap();

    let del_resp = app.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);

    // 7. Verification: /v1/auth/me should now return 401 Unauthorized
    let me_req = Request::builder()
        .method("GET")
        .uri("/v1/auth/me")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let me_resp = app.clone().oneshot(me_req).await.unwrap();
    assert_eq!(me_resp.status(), StatusCode::UNAUTHORIZED);
}
