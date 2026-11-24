use axum::Router;
mod common;
use cabinet_api::{
    infra::db::create_db_pool,
    modules::{auth::handlers::router as auth_router, bookmarks::handlers::router as bookmarks_router, folders::handlers::router as folders_router},
};
use common::{call, register};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn unsorted_lists_only_items_outside_folders() {
    let config = common::config();
    let pool = create_db_pool(&config).await.expect("Isolated test database is required");
    let app = Router::new()
        .nest("/v1/auth", auth_router())
        .nest("/v1/bookmarks", bookmarks_router())
        .nest("/v1/folders", folders_router())
        .with_state(pool);

    let user = register(&app, "unsorted").await;
    let (_, folder) = call(&app, "POST", "/v1/folders", Some(&user), Some(json!({ "name": "Filed" }))).await;
    for (title, folder_id) in [("Loose", None), ("Filed item", folder["id"].as_str())] {
        let (status, body) = call(&app, "POST", "/v1/bookmarks", Some(&user), Some(json!({
            "url": format!("https://example.com/{}", Uuid::now_v7()),
            "title": title,
            "itemType": "LINK",
            "folderId": folder_id,
            "clientRequestId": Uuid::now_v7(),
        }))).await;
        assert!(status.is_success(), "{status} {body}");
    }

    let (_, list) = call(&app, "GET", "/v1/bookmarks?unsorted=true", Some(&user), None).await;
    let titles: Vec<&str> = list["items"].as_array().unwrap().iter().map(|b| b["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Loose"]);
    assert_eq!(list["totalCount"], 1);

    // Without the flag both stay listed.
    let (_, list) = call(&app, "GET", "/v1/bookmarks", Some(&user), None).await;
    assert_eq!(list["totalCount"], 2);
}
