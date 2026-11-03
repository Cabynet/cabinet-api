mod common;
use axum::{body::{Body,to_bytes},http::{Request,StatusCode},Router};
use cabinet_api::{infra::db::create_db_pool,modules};
use serde_json::{json,Value};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(app:&Router,method:&str,path:&str,token:Option<&str>,body:Value,status:u16)->Value{
    let mut request=Request::builder().method(method).uri(path);
    if let Some(token)=token{request=request.header("Authorization",format!("Bearer {token}"));}
    let request=if body.is_null(){request.body(Body::empty())}else{request.header("Content-Type","application/json").body(Body::from(body.to_string()))}.unwrap();
    let response=app.clone().oneshot(request).await.unwrap();let actual=response.status();
    let bytes=to_bytes(response.into_body(),4*1024*1024).await.unwrap();
    assert_eq!(actual,StatusCode::from_u16(status).unwrap(),"{method} {path}: {}",String::from_utf8_lossy(&bytes));
    if bytes.is_empty(){Value::Null}else{serde_json::from_slice(&bytes).unwrap()}
}
async fn account(app:&Router)->Value{call(app,"POST","/v1/auth/register",None,json!({"email":format!("contract-{}@example.com",Uuid::now_v7()),"name":"Contract test","password":"ContractOnly!123"}),201).await}
#[tokio::test]
async fn frontend_backend_contract_and_authorization(){
    let pool=create_db_pool(&common::config()).await.unwrap();
    let app=Router::new().nest("/v1/auth",modules::auth::handlers::router()).nest("/v1/bookmarks",modules::bookmarks::handlers::router()).nest("/v1/folders",modules::folders::handlers::router()).nest("/v1/tags",modules::tags::handlers::router()).nest("/v1/share",modules::folders::handlers::public_share_router()).nest("/v1/activity",modules::activity::router()).nest("/v1/operations",modules::operations::router()).with_state(pool);
    let owner=account(&app).await;let member=account(&app).await;let outsider=account(&app).await;
    let a=owner["token"].as_str();let b=member["token"].as_str();let c=outsider["token"].as_str();
    let folder=call(&app,"POST","/v1/folders",a,json!({"name":"Shared references"}),201).await;
    let fid=folder["id"].as_str().unwrap();
    let item=call(&app,"POST","/v1/bookmarks",a,json!({"title":"Alpha reference","url":"https://example.com/alpha","folder_id":fid,"tags":["private-tag"]}),201).await;
    let id=item["id"].as_str().unwrap();
    call(&app,"GET",&format!("/v1/bookmarks/{id}"),c,Value::Null,404).await;
    call(&app,"POST",&format!("/v1/folders/{fid}/members"),a,json!({"email":member["user"]["email"],"role":"VIEWER"}),200).await;
    let shared=call(&app,"GET",&format!("/v1/bookmarks/{id}"),b,Value::Null,200).await;
    assert_eq!(shared["access"],"viewer");assert_eq!(shared["tags"],json!([]));
    call(&app,"PATCH",&format!("/v1/bookmarks/{id}"),b,json!({"title":"Forbidden"}),403).await;
    call(&app,"POST","/v1/bookmarks",b,json!({"title":"Forbidden","folder_id":fid}),403).await;
    call(&app,"PATCH",&format!("/v1/folders/{fid}/members/{}",member["user"]["id"].as_str().unwrap()),a,json!({"role":"EDITOR"}),200).await;
    call(&app,"PATCH",&format!("/v1/bookmarks/{id}"),b,json!({"title":"Alpha edited"}),200).await;
    call(&app,"PATCH",&format!("/v1/bookmarks/{id}"),b,json!({"is_archived":true}),403).await;
    let folders=call(&app,"GET","/v1/folders",b,Value::Null,200).await;
    assert_eq!(folders[0]["access"],"EDITOR");
    let me=call(&app,"GET","/v1/auth/me",b,Value::Null,200).await;
    assert!(me["session"]["createdAt"].is_string());
    assert_eq!(me["session"]["userId"],member["user"]["id"]);
    // Scoped personal queue stays personal to user b:
    assert_eq!(call(&app,"GET","/v1/bookmarks",b,Value::Null,200).await["totalCount"],0);
    // Unscoped global search finds items in accessible shared folders:
    assert_eq!(call(&app,"GET","/v1/bookmarks?search=Alpha",b,Value::Null,200).await["totalCount"],1);
    let rows=call(&app,"GET",&format!("/v1/bookmarks?folder_id={fid}&search=Alpha&sort=relevance&summary=true"),b,Value::Null,200).await;
    assert_eq!(rows["totalCount"],1);assert_eq!(rows["items"][0]["access"],"editor");
    for sort in ["newest","oldest","title","domain","custom","relevance"]{call(&app,"GET",&format!("/v1/bookmarks?sort={sort}"),a,Value::Null,200).await;}
    call(&app,"POST","/v1/bookmarks/reorder",a,json!({"ids":[id]}),200).await;
    let note=Uuid::now_v7().to_string();let save=Uuid::now_v7();
    let body=json!({"base_revision":0,"save_id":save,"document":{"type":"doc","content":[]},"plain_text":"Private notes","title":"Secret note","folder_id":fid,"set_folder":true,"tags":["notes"]});
    let saved=call(&app,"PUT",&format!("/v1/bookmarks/{note}/note"),a,body.clone(),200).await;assert_eq!(saved["revision"],1);
    let replay=call(&app,"PUT",&format!("/v1/bookmarks/{note}/note"),a,body,200).await;assert_eq!(replay["revision"],1);
    call(&app,"PUT",&format!("/v1/bookmarks/{note}/note"),a,json!({"base_revision":0,"save_id":Uuid::now_v7(),"plain_text":"Stale"}),409).await;
    let share=call(&app,"POST",&format!("/v1/folders/{fid}/share"),a,Value::Null,200).await;
    let token=share["shareToken"].as_str().unwrap();assert_eq!(token.len(),64);
    let public=call(&app,"GET",&format!("/v1/share/{token}"),None,Value::Null,200).await;
    assert_eq!(public["bookmarks"].as_array().unwrap().len(),1);assert!(!public.to_string().contains("Private notes"));
    let copied=call(&app,"POST",&format!("/v1/share/{token}/copy"),c,Value::Null,200).await;assert_eq!(copied["copied"],1);
    call(&app,"POST","/v1/bookmarks/bulk/move",a,json!({"ids":[id],"folderId":copied["folderId"]}),404).await;
    call(&app,"POST","/v1/bookmarks/bulk/move",a,json!({"ids":[id],"folderId":null}),200).await;
    let moved=call(&app,"GET",&format!("/v1/bookmarks/{id}"),a,Value::Null,200).await;assert!(moved["folderId"].is_null());
    call(&app,"GET",&format!("/v1/bookmarks/{id}"),b,Value::Null,404).await;
    call(&app,"POST","/v1/bookmarks/bulk/archive",a,json!({"ids":[id],"isArchived":true}),200).await;
    let archived=call(&app,"GET","/v1/bookmarks?is_archived=true",a,Value::Null,200).await;assert_eq!(archived["items"][0]["id"],id);
    call(&app,"DELETE",&format!("/v1/bookmarks/{id}/permanent"),a,Value::Null,400).await;
    call(&app,"POST","/v1/bookmarks/bulk/trash",a,json!({"ids":[id]}),200).await;
    let trash=call(&app,"GET","/v1/bookmarks?trash=true",a,Value::Null,200).await;assert_eq!(trash["totalCount"],1);
    call(&app,"POST",&format!("/v1/bookmarks/{id}/restore"),a,Value::Null,200).await;
    call(&app,"POST","/v1/bookmarks/bulk/tags",a,json!({"ids":[id],"tags":["merge-source","merge-target"]}),200).await;
    let tags=call(&app,"GET","/v1/tags",a,Value::Null,200).await;
    let tag_id=|name:&str|tags.as_array().unwrap().iter().find(|t|t["name"]==name).unwrap()["id"].as_str().unwrap().to_string();
    call(&app,"POST",&format!("/v1/tags/{}/merge",tag_id("merge-source")),a,json!({"targetId":tag_id("merge-target")}),200).await;
    call(&app,"PATCH",&format!("/v1/tags/{}",tag_id("merge-target")),a,json!({"name":"renamed"}),200).await;
    call(&app,"DELETE","/v1/tags/unused",a,Value::Null,200).await;
    let drawing=Uuid::now_v7();
    call(&app,"PUT",&format!("/v1/bookmarks/{drawing}/drawing"),a,json!({"title":"Sketch","scene":{"elements":[],"appState":{},"files":{}}}),200).await;
    let loaded=call(&app,"GET",&format!("/v1/bookmarks/{drawing}"),a,Value::Null,200).await;assert_eq!(loaded["itemType"],"DRAWING");assert!(loaded["drawing"].is_object());
    let instant=call(&app,"POST","/v1/bookmarks",a,json!({"title":"Full metadata","description":"Complete","url":"https://example.com/instant","thumbnail_url":"https://example.com/thumb.webp"}),201).await;
    assert_eq!(instant["metadataStatus"],"COMPLETED");
    call(&app,"GET","/v1/activity",a,Value::Null,200).await;
    let health_resp=call(&app,"GET","/v1/bookmarks/health",a,Value::Null,200).await;
    assert!(health_resp["ok"].is_number());
    assert!(health_resp["healthy"].is_number());
    assert!(health_resp["redirected"].is_number());
    assert!(health_resp["trash"].is_number());
    assert_eq!(call(&app,"GET","/v1/operations/access",a,Value::Null,200).await["enabled"],false);
    call(&app,"GET","/v1/operations/metrics",a,Value::Null,403).await;
    assert_eq!(call(&app,"GET","/v1/auth/accounts",a,Value::Null,200).await,json!([]));
    call(&app,"DELETE",&format!("/v1/folders/{fid}/share"),a,Value::Null,200).await;
    call(&app,"GET",&format!("/v1/share/{token}"),None,Value::Null,404).await;
}
