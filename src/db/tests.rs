use super::*;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn pool_is_lazy_and_rejects_invalid_configuration() {
    let pool = pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).expect("lazy pool");
    assert_eq!(pool.status().size, 0);
    assert!(super::pool("not a PostgreSQL connection string", 1).is_err());
    assert!(super::pool("postgres://localhost/test", 0).is_err());
}

#[derive(Debug, PartialEq)]
struct CodecRow {
    id: Uuid,
    text: String,
    optional: Option<String>,
    value: Value,
    timestamp: DateTime<Utc>,
    bytes: Vec<u8>,
    count: i64,
    quantity: i32,
    small: i16,
    single: f32,
    double: f64,
    ids: Vec<Uuid>,
}
row!(CodecRow {
    id,
    text,
    optional,
    value,
    timestamp,
    bytes,
    count,
    quantity,
    small,
    single,
    double,
    ids
});

#[tokio::test]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_codecs_transactions_errors_and_pool_reuse() -> anyhow::Result<()> {
    let database_url = std::env::var("TEST_DATABASE_URL")?;
    let pool = pool(&database_url, 1)?;
    let mut connection = pool.get().await?;
    let expected = CodecRow {
        id: Uuid::new_v4(),
        text: "과자".into(),
        optional: None,
        value: json!({"weight_grams": 42}),
        timestamp: Utc::now(),
        bytes: vec![0, 1, 255],
        count: 3_000_000_000,
        quantity: 3,
        small: 2,
        single: 0.75,
        double: 37.1234,
        ids: vec![Uuid::new_v4()],
    };
    // PostgreSQL timestamps retain microseconds, matching the previous driver.
    let expected = CodecRow {
        timestamp: DateTime::from_timestamp_micros(expected.timestamp.timestamp_micros())
            .expect("valid fixture timestamp"),
        ..expected
    };
    let actual = fetch_one::<CodecRow>(&connection,
        "SELECT $2::text AS text, $1::uuid AS id, $3::text AS optional, $4::jsonb AS value, $5::timestamptz AS timestamp, $6::bytea AS bytes, $7::bigint AS count, $8::integer AS quantity, $9::smallint AS small, $10::real AS single, $11::double precision AS double, $12::uuid[] AS ids",
        &[&expected.id, &expected.text, &expected.optional, &expected.value, &expected.timestamp,
          &expected.bytes, &expected.count, &expected.quantity, &expected.small,
          &expected.single, &expected.double, &expected.ids]).await?;
    assert_eq!(actual, expected);
    assert_eq!(
        scalar_optional::<Option<String>>(&connection, "SELECT NULL::text", &[]).await?,
        Some(None)
    );
    assert_eq!(
        scalar_optional::<i32>(&connection, "SELECT 1 WHERE false", &[]).await?,
        None
    );
    assert!(
        scalar_one::<i32>(&connection, "SELECT 1 WHERE false", &[])
            .await
            .is_err()
    );
    assert!(
        scalar_one::<i64>(&connection, "SELECT 1::integer", &[])
            .await
            .is_err()
    );
    let tuple = fetch_one::<(String, Option<Uuid>, i32)>(
        &connection,
        "SELECT 'tuple'::text, NULL::uuid, 3::integer",
        &[],
    )
    .await?;
    assert_eq!(tuple, ("tuple".into(), None, 3));
    assert_eq!(
        scalar_all::<i32>(&connection, "SELECT generate_series(1,3)", &[]).await?,
        vec![1, 2, 3]
    );
    execute(
        &connection,
        "CREATE TEMP TABLE driver_atomicity (id integer PRIMARY KEY, value text NOT NULL)",
        &[],
    )
    .await?;
    {
        let tx = connection.transaction().await?;
        assert_eq!(
            execute(
                &tx,
                "INSERT INTO driver_atomicity VALUES (1, 'committed')",
                &[]
            )
            .await?,
            1
        );
        tx.commit().await?;
    }
    {
        let tx = connection.transaction().await?;
        execute(
            &tx,
            "INSERT INTO driver_atomicity VALUES (2, 'rolled back on drop')",
            &[],
        )
        .await?;
    }
    assert_eq!(
        scalar_one::<i64>(&connection, "SELECT count(*) FROM driver_atomicity", &[]).await?,
        1
    );
    {
        let tx = connection.transaction().await?;
        execute(
            &tx,
            "INSERT INTO driver_atomicity VALUES (3, 'error rollback')",
            &[],
        )
        .await?;
        let error = execute(
            &tx,
            "INSERT INTO driver_atomicity VALUES (1, 'duplicate')",
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
        );
        assert_eq!(
            error.as_db_error().and_then(|e| e.constraint()),
            Some("driver_atomicity_pkey")
        );
        tx.rollback().await?;
    }
    assert_eq!(
        scalar_one::<i64>(&connection, "SELECT count(*) FROM driver_atomicity", &[]).await?,
        1
    );
    drop(connection);
    let connection = pool.get().await?;
    assert_eq!(
        scalar_one::<String>(
            &connection,
            "SELECT value FROM driver_atomicity WHERE id=1",
            &[]
        )
        .await?,
        "committed"
    );
    execute(&connection, "DROP TABLE driver_atomicity", &[]).await?;
    drop(connection);
    crate::routes::scheduling::run_once(&pool).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_TLS_DATABASE_URL with a private test CA"]
async fn postgres_tls_verifies_trust_and_encrypts() -> anyhow::Result<()> {
    let url = std::env::var("TEST_TLS_DATABASE_URL")?;
    assert!(
        url.contains("sslmode=require"),
        "TLS test must require encryption"
    );
    let pool = pool(&url, 1)?;
    if std::env::var("TEST_TLS_EXPECT_UNTRUSTED").as_deref() == Ok("1") {
        let error = pool
            .get()
            .await
            .expect_err("private CA server must be rejected without trust");
        assert!(format!("{error:?}").contains("UnknownIssuer"), "{error:?}");
    } else {
        let connection = pool.get().await?;
        assert!(
            scalar_one::<bool>(
                &connection,
                "SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()",
                &[]
            )
            .await?
        );
    }
    Ok(())
}
