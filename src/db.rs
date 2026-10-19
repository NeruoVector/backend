use sqlx::PgPool;

pub async fn ping(pool: &PgPool) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
}
