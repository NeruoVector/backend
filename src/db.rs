use deadpool_postgres::GenericClient;
use tokio_postgres::{
    Error, Row,
    types::{FromSqlOwned, ToSql},
};
pub type PgPool = deadpool_postgres::Pool;

/// Pool construction is lazy; callers that require readiness should acquire a connection.
pub fn pool(database_url: &str, max_connections: u32) -> anyhow::Result<PgPool> {
    anyhow::ensure!(max_connections > 0, "database pool size must be positive");
    let config: tokio_postgres::Config = database_url.parse()?;
    let certificates = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(certificates.certs);
    anyhow::ensure!(!roots.is_empty(), "cannot load database TLS trust roots");
    for warning in certificates.errors {
        tracing::warn!(?warning, "database TLS trust root could not be loaded");
    }
    let tls_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tls = tokio_postgres_rustls::MakeRustlsConnect::new(tls_config);
    let manager = deadpool_postgres::Manager::from_config(
        config,
        tls,
        deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Verified,
        },
    );
    Ok(PgPool::builder(manager)
        .max_size(max_connections as usize)
        .runtime(deadpool_postgres::Runtime::Tokio1)
        .wait_timeout(Some(std::time::Duration::from_secs(30)))
        .create_timeout(Some(std::time::Duration::from_secs(30)))
        .build()?)
}
pub async fn ping(pool: &PgPool) -> Result<i32, crate::error::AppError> {
    Ok(scalar_one(&pool.get().await?, "SELECT 1", &[]).await?)
}
/// Domain rows decode by name so SELECT order cannot silently change meaning.
pub trait FromRow: Sized {
    fn from_row(row: &Row) -> Result<Self, Error>;
}
macro_rules! row {
    ($name:ty { $($field:ident),* $(,)? }) => {
        impl crate::db::FromRow for $name {
            fn from_row(row: &tokio_postgres::Row) -> Result<Self, tokio_postgres::Error> {
                Ok(Self { $($field: row.try_get(stringify!($field))?),* })
            }
        }
    };
}
pub(crate) use row;
impl<A: FromSqlOwned, B: FromSqlOwned> FromRow for (A, B) {
    fn from_row(row: &Row) -> Result<Self, Error> {
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
}
impl<A: FromSqlOwned, B: FromSqlOwned, C: FromSqlOwned> FromRow for (A, B, C) {
    fn from_row(row: &Row) -> Result<Self, Error> {
        Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
    }
}
pub async fn fetch_one<T: FromRow>(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<T, Error> {
    T::from_row(
        &client
            .query_one(&client.prepare_cached(sql).await?, params)
            .await?,
    )
}
pub async fn fetch_optional<T: FromRow>(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Option<T>, Error> {
    client
        .query_opt(&client.prepare_cached(sql).await?, params)
        .await?
        .as_ref()
        .map(T::from_row)
        .transpose()
}
pub async fn fetch_all<T: FromRow>(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Vec<T>, Error> {
    client
        .query(&client.prepare_cached(sql).await?, params)
        .await?
        .iter()
        .map(T::from_row)
        .collect()
}
pub async fn scalar_one<T: FromSqlOwned>(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<T, Error> {
    client
        .query_one(&client.prepare_cached(sql).await?, params)
        .await?
        .try_get(0)
}
pub async fn scalar_optional<T: FromSqlOwned>(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Option<T>, Error> {
    client
        .query_opt(&client.prepare_cached(sql).await?, params)
        .await?
        .map(|row| row.try_get(0))
        .transpose()
}
pub async fn scalar_all<T: FromSqlOwned>(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Vec<T>, Error> {
    client
        .query(&client.prepare_cached(sql).await?, params)
        .await?
        .iter()
        .map(|row| row.try_get(0))
        .collect()
}
pub async fn execute(
    client: &impl GenericClient,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<u64, Error> {
    client
        .execute(&client.prepare_cached(sql).await?, params)
        .await
}

#[cfg(test)]
mod tests;
