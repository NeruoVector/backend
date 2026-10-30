use deadpool_postgres::Transaction;
use uuid::Uuid;

use super::planner::DroneCapacity;

#[derive(Debug)]
pub(super) struct CandidateRow {
    pub id: Uuid,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub complete_weights: bool,
    pub estimated_weight_grams: Option<i64>,
}
crate::db::row!(CandidateRow {
    id,
    latitude,
    longitude,
    created_at,
    complete_weights,
    estimated_weight_grams
});

#[derive(Debug)]
struct FleetRow {
    drone_id: String,
    capacity_grams: i32,
    home_latitude: f64,
    home_longitude: f64,
    cruise_speed_mps: f64,
    max_altitude_m: f64,
    available_at: chrono::DateTime<chrono::Utc>,
}
crate::db::row!(FleetRow {
    drone_id,
    capacity_grams,
    home_latitude,
    home_longitude,
    cruise_speed_mps,
    max_altitude_m,
    available_at
});

impl From<FleetRow> for DroneCapacity {
    fn from(row: FleetRow) -> Self {
        Self {
            drone_id: row.drone_id,
            capacity_grams: i64::from(row.capacity_grams),
            home_latitude: row.home_latitude,
            home_longitude: row.home_longitude,
            cruise_speed_mps: row.cruise_speed_mps,
            max_altitude_m: row.max_altitude_m,
            available_at: row.available_at,
        }
    }
}

pub(super) async fn synchronize_assigned_orders(
    tx: &mut Transaction<'_>,
) -> Result<u64, tokio_postgres::Error> {
    crate::db::execute(
        &*tx,
        r#"
        UPDATE neurovector.orders o
        SET status = 'assigned', updated_at = now()
        WHERE o.status = 'ready'
          AND o.delivery_id IS NOT NULL
          AND EXISTS (
              SELECT 1 FROM neurovector.delivery_stops s
              WHERE s.order_id = o.id AND s.status = 'scheduled'
          )
        "#,
        &[],
    )
    .await
}

pub(super) async fn lock_candidates(
    tx: &mut Transaction<'_>,
    limit: i64,
) -> Result<Vec<CandidateRow>, tokio_postgres::Error> {
    crate::db::fetch_all::<CandidateRow>(&*tx, r#"
        WITH locked AS (
            SELECT o.id, o.latitude, o.longitude, o.created_at
            FROM neurovector.orders o
            WHERE o.status IN ('paid', 'ready') AND o.delivery_id IS NULL
            ORDER BY o.created_at, o.id
            LIMIT $1
            FOR UPDATE OF o SKIP LOCKED
        )
        SELECT locked.id, locked.latitude, locked.longitude, locked.created_at,
               COALESCE(bool_and(oi.weight_grams IS NOT NULL AND oi.weight_grams > 0), false) AS complete_weights,
               sum(oi.weight_grams::bigint * oi.quantity::bigint)::bigint AS estimated_weight_grams
        FROM locked
        LEFT JOIN neurovector.order_items oi ON oi.order_id = locked.id
        GROUP BY locked.id, locked.latitude, locked.longitude, locked.created_at
        ORDER BY locked.created_at, locked.id
        "#, &[&(limit)])
    .await
}

pub(super) async fn load_active_fleet(
    tx: &mut Transaction<'_>,
    turnaround_seconds: i64,
    telemetry_online_ttl_seconds: i64,
) -> Result<Vec<DroneCapacity>, tokio_postgres::Error> {
    let rows = crate::db::fetch_all::<FleetRow>(
        &*tx,
        r#"
        SELECT f.drone_id, f.capacity_grams, f.home_latitude, f.home_longitude,
               f.cruise_speed_mps, f.max_altitude_m,
               GREATEST(
                   now(),
                   COALESCE(
                       max(d.scheduled_at + make_interval(secs => r.estimated_duration_s)
                           + make_interval(secs => $1::double precision)),
                       now()
                   )
               ) AS available_at
        FROM neurovector.delivery_fleet f
        CROSS JOIN LATERAL (
            SELECT t.ts, t.received_at, t.status
            FROM neurovector.current_drone_telemetry t
            WHERE t.drone_id = f.drone_id
            ORDER BY t.received_at DESC, t.ts DESC, t.id DESC
            LIMIT 1
        ) latest
        LEFT JOIN neurovector.deliveries d
          ON d.drone_id = f.drone_id
         AND d.status IN ('scheduled', 'dispatched', 'in_flight')
        LEFT JOIN neurovector.routes r ON r.id = d.route_id
        WHERE f.active
          AND latest.ts >= now() - make_interval(secs => $2::double precision)
          AND latest.ts <= now() + interval '5 seconds'
          AND latest.received_at >= now() - make_interval(secs => $2::double precision)
          AND latest.received_at <= now() + interval '5 seconds'
          AND lower(COALESCE(latest.status, '')) NOT IN ('failed', 'aborted')
        GROUP BY f.drone_id, f.capacity_grams, f.home_latitude, f.home_longitude,
                 f.cruise_speed_mps, f.max_altitude_m
        ORDER BY available_at, f.drone_id
        "#,
        &[
            &(turnaround_seconds as f64),
            &(telemetry_online_ttl_seconds as f64),
        ],
    )
    .await?;
    Ok(rows.into_iter().map(DroneCapacity::from).collect())
}
