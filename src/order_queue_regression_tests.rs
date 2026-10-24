use super::*;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set; skipping PostgreSQL integration test");
        return None;
    };
    Some(PgPool::connect(&url).await.expect("test database"))
}

#[tokio::test]
async fn recovers_abandoned_jobs_but_skips_active_locks_and_exhausted_jobs() {
    let Some(pool) = pool().await else { return };
    let mut ids = Vec::new();
    for attempts in [1, 1, 3] {
        ids.push(
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO neurovector.order_jobs(payload, status, attempts, started_at)
             VALUES ('{}', 'processing', $1, now() - interval '11 minutes') RETURNING id",
            )
            .bind(attempts)
            .fetch_one(&pool)
            .await
            .expect("abandoned job"),
        );
    }
    let mut active = pool.begin().await.expect("active worker transaction");
    sqlx::query("SELECT id FROM neurovector.order_jobs WHERE id = $1 FOR UPDATE")
        .bind(ids[1])
        .execute(&mut *active)
        .await
        .expect("lock active job");

    recover_stale_jobs(&pool).await.expect("periodic recovery");

    for (id, expected) in ids.iter().zip(["queued", "processing", "dead"]) {
        assert_eq!(get_job(&pool, *id).await.expect("job").status, expected);
    }
    active.rollback().await.expect("release active job");
    recover_stale_jobs(&pool)
        .await
        .expect("recover released job");
    assert_eq!(get_job(&pool, ids[1]).await.expect("job").status, "queued");
    let state = crate::state::test_state(pool.clone());
    process_create_order_job(&state, ids[2])
        .await
        .expect("exhausted job is ignored");
    assert_eq!(get_job(&pool, ids[2]).await.expect("job").attempts, 3);
    sqlx::query("DELETE FROM neurovector.order_jobs WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn retries_failed_jobs_until_the_attempt_limit() {
    let Some(pool) = pool().await else { return };
    let state = crate::state::test_state(pool.clone());
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO neurovector.order_jobs(payload) VALUES ('{}') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("invalid job");

    for expected in ["failed", "failed", "dead"] {
        process_create_order_job(&state, id)
            .await
            .expect("process invalid job");
        assert_eq!(get_job(&pool, id).await.expect("job").status, expected);
    }

    process_create_order_job(&state, id)
        .await
        .expect("dead job is a no-op");
    assert_eq!(get_job(&pool, id).await.expect("job").attempts, 3);
    sqlx::query("DELETE FROM neurovector.order_jobs WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn outbox_event_rolls_back_with_the_order_transaction() {
    let Some(pool) = pool().await else { return };
    let order_id = Uuid::new_v4();
    let mut tx = pool.begin().await.expect("begin transaction");
    record_order_event(&mut tx, "test.rollback", order_id, None, json!({}))
        .await
        .expect("record event");
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM neurovector.order_events WHERE order_id = $1 AND published_at IS NULL",
    ).bind(order_id).fetch_one(&mut *tx).await.expect("pending outbox");
    assert_eq!(pending, 1);

    tx.rollback().await.expect("failed business transaction");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM neurovector.order_events WHERE order_id = $1")
            .bind(order_id)
            .fetch_one(&pool)
            .await
            .expect("rolled back outbox");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn ignores_failure_from_a_worker_that_lost_its_attempt() {
    let Some(pool) = pool().await else { return };
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO neurovector.order_jobs(payload,status,attempts,started_at)
         VALUES ('{}','processing',2,now()) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("new worker owns attempt two");

    fail_job(&pool, id, 1, "late failure from attempt one".into())
        .await
        .expect("ignore old worker");

    let job = get_job(&pool, id).await.expect("job");
    assert_eq!(job.status, "processing");
    assert_eq!(job.last_error, None);
    sqlx::query("DELETE FROM neurovector.order_jobs WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("cleanup");
}
