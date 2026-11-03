mod common;
use cabinet_api::{infra::db::create_db_pool, modules::tasks::service::spawn_heartbeat};
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn heartbeat_keeps_a_running_task_fresh_only_for_its_attempt() {
    let pool = create_db_pool(&common::config()).await.expect("Isolated test database is required");
    let user = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, name) VALUES ($1, $2, 'Heartbeat')")
        .bind(user)
        .bind(format!("heartbeat_{user}@example.com"))
        .execute(&pool)
        .await
        .unwrap();
    let task = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO background_tasks (id, user_id, kind, status, attempt, updated_at) VALUES ($1, $2, 'IMPORT_HTML', 'RUNNING', 2, now() - interval '10 minutes')",
    )
    .bind(task)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    let stale = |attempt: i32| spawn_heartbeat(pool.clone(), task, attempt, Duration::from_millis(50));
    let age = || async {
        sqlx::query_scalar::<_, f64>("SELECT extract(epoch FROM now() - updated_at)::float8 FROM background_tasks WHERE id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    // A heartbeat for a superseded attempt must not keep the row alive.
    let wrong = stale(1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    wrong.abort();
    assert!(age().await > 500.0);

    let right = stale(2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    right.abort();
    assert!(age().await < 5.0, "heartbeat should refresh updated_at");
}
