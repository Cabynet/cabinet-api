use axum::{http::StatusCode, Router};
mod common;
use cabinet_api::{
    infra::db::create_db_pool,
    modules::{auth::handlers::router as auth_router, folders::handlers::router as folders_router},
};
use common::{call, register};
use serde_json::{json, Value};
use uuid::Uuid;

fn pending(folders: &Value, id: &str) -> Option<i64> {
    folders.as_array().unwrap().iter().find(|f| f["id"] == id).map(|f| f["pendingRequests"].as_i64().unwrap())
}

/// Request access, approve or deny, and leave — like Google Drive.
#[tokio::test]
async fn request_access_approve_deny_and_leave() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");
    let app = Router::new().nest("/v1/auth", auth_router()).nest("/v1/folders", folders_router()).with_state(pool);

    let owner = register(&app, "req_owner").await;
    let alice = register(&app, "req_alice").await;
    let bob = register(&app, "req_bob").await;
    let (_, folder) = call(&app, "POST", "/v1/folders", Some(&owner), Some(json!({ "name": "Research" }))).await;
    let id = folder["id"].as_str().unwrap().to_string();
    let requests = format!("/v1/folders/{id}/access-requests");

    // Someone without access asks once; asking again only refreshes the note.
    let (_, mine) = call(&app, "GET", &format!("{requests}/mine"), Some(&alice), None).await;
    assert_eq!(mine["requested"], false);
    let (status, body) = call(&app, "POST", &requests, Some(&alice), Some(json!({ "message": "For the thesis" }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    call(&app, "POST", &requests, Some(&alice), Some(json!({ "message": "  Please  " }))).await;
    call(&app, "POST", &requests, Some(&bob), Some(json!({}))).await;
    let (_, mine) = call(&app, "GET", &format!("{requests}/mine"), Some(&alice), None).await;
    assert_eq!(mine["requested"], true);
    let (status, _) = call(&app, "POST", &requests, Some(&alice), Some(json!({ "message": "x".repeat(501) }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, "POST", &format!("/v1/folders/{}/access-requests", Uuid::now_v7()), Some(&alice), Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Only the owner sees and handles requests; the folder list counts them for the owner.
    let (status, _) = call(&app, "GET", &requests, Some(&alice), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, list) = call(&app, "GET", &requests, Some(&owner), None).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["message"], "Please");
    assert!(list[1]["message"].is_null());
    let alice_id = list[0]["userId"].as_str().unwrap().to_string();
    let bob_id = list[1]["userId"].as_str().unwrap().to_string();
    let (_, folders) = call(&app, "GET", "/v1/folders", Some(&owner), None).await;
    assert_eq!(pending(&folders, &id), Some(2));

    // Approving with a role adds the person; denying just clears the request.
    let (status, _) = call(&app, "POST", &format!("{requests}/{alice_id}/approve"), Some(&owner), Some(json!({ "role": "OWNER" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = call(&app, "POST", &format!("{requests}/{alice_id}/approve"), Some(&owner), Some(json!({ "role": "EDITOR" }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = call(&app, "POST", &format!("{requests}/{alice_id}/approve"), Some(&owner), Some(json!({ "role": "EDITOR" }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, folders) = call(&app, "GET", "/v1/folders", Some(&alice), None).await;
    let shared = folders.as_array().unwrap().iter().find(|f| f["id"] == id.as_str()).unwrap();
    assert_eq!(shared["access"], "EDITOR");
    assert_eq!(shared["pendingRequests"], 0, "only owners see the count");
    let (status, _) = call(&app, "POST", &requests, Some(&alice), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "members don't need to ask");

    call(&app, "DELETE", &format!("{requests}/{bob_id}"), Some(&owner), None).await;
    let (_, list) = call(&app, "GET", &requests, Some(&owner), None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    let (_, mine) = call(&app, "GET", &format!("{requests}/mine"), Some(&bob), None).await;
    assert_eq!(mine["requested"], false);

    // A member can leave; the owner can't leave their own folder, and outsiders get 404.
    let membership = format!("/v1/folders/{id}/membership");
    let (status, _) = call(&app, "DELETE", &membership, Some(&owner), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, "DELETE", &membership, Some(&bob), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", &membership, Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, folders) = call(&app, "GET", "/v1/folders", Some(&alice), None).await;
    assert!(folders.as_array().unwrap().iter().all(|f| f["id"] != id.as_str()));
}
