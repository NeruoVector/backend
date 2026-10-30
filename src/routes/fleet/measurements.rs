use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};

use uuid::Uuid;

use crate::{auth::CurrentAdmin, error::AppError, state::AppState};

use super::types::{PayloadMeasurement, PayloadMeasurementInput};

#[derive(Debug)]
struct DeliveryWeightContext {
    estimated_weight_grams: Option<i64>,
    capacity_grams: i32,
}
crate::db::row!(DeliveryWeightContext {
    estimated_weight_grams,
    capacity_grams
});

#[derive(Debug)]
struct MeasurementRow {
    id: Uuid,
    delivery_id: Uuid,
    drone_id: String,
    weight_grams: i32,
    capacity_exceeded: bool,
    measured_by: String,
    measured_at: DateTime<Utc>,
}
crate::db::row!(MeasurementRow {
    id,
    delivery_id,
    drone_id,
    weight_grams,
    capacity_exceeded,
    measured_by,
    measured_at
});

pub(super) async fn record_payload_measurement(
    State(state): State<AppState>,
    Extension(admin): Extension<CurrentAdmin>,
    Path(drone_id): Path<String>,
    Json(body): Json<PayloadMeasurementInput>,
) -> Result<(StatusCode, Json<PayloadMeasurement>), AppError> {
    if body.weight_grams < 0 {
        return Err(AppError::BadRequest(
            "weight_grams must be zero or greater".into(),
        ));
    }

    let mut connection = state.pool.get().await?;
    let tx = connection.transaction().await?;
    let context = crate::db::fetch_optional::<DeliveryWeightContext>(
        &tx,
        r#"
        SELECT d.estimated_weight_grams, f.capacity_grams
        FROM neurovector.deliveries d
        JOIN neurovector.delivery_fleet f ON f.drone_id = d.drone_id
        WHERE d.id = $1 AND d.drone_id = $2
          AND d.status IN ('scheduled', 'dispatched', 'in_flight')
        FOR UPDATE OF d
        "#,
        &[&(body.delivery_id), &(&drone_id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    let capacity_exceeded = body.weight_grams > context.capacity_grams;
    let measurement = crate::db::fetch_one::<MeasurementRow>(
        &tx,
        r#"
        INSERT INTO neurovector.delivery_weight_measurements
            (delivery_id, drone_id, weight_grams, capacity_exceeded, measured_by)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, delivery_id, drone_id, weight_grams, capacity_exceeded,
                  measured_by, measured_at
        "#,
        &[
            &(body.delivery_id),
            &(&drone_id),
            &(body.weight_grams),
            &(capacity_exceeded),
            &(&admin.username),
        ],
    )
    .await?;
    crate::db::execute(
        &tx,
        r#"
        INSERT INTO neurovector.order_tracking_events
            (order_id, delivery_stop_id, event_type, payload)
        SELECT s.order_id, s.id, 'payload_weight_measured',
               jsonb_build_object(
                   'measurement_id', $2::uuid,
                   'weight_grams', $3::integer,
                   'capacity_grams', $4::integer,
                   'capacity_exceeded', $5::boolean,
                   'measured_by', $6::text,
                   'measured_at', $7::timestamptz
               )
        FROM neurovector.delivery_stops s
        WHERE s.delivery_id = $1
        "#,
        &[
            &(body.delivery_id),
            &(measurement.id),
            &(measurement.weight_grams),
            &(context.capacity_grams),
            &(measurement.capacity_exceeded),
            &(&measurement.measured_by),
            &(measurement.measured_at),
        ],
    )
    .await?;
    tx.commit().await?;
    drop(connection);

    let response = PayloadMeasurement {
        id: measurement.id,
        delivery_id: measurement.delivery_id,
        drone_id: measurement.drone_id,
        estimated_delivery_weight_grams: context.estimated_weight_grams,
        weight_grams: measurement.weight_grams,
        capacity_grams: context.capacity_grams,
        capacity_exceeded: measurement.capacity_exceeded,
        measured_by: measurement.measured_by,
        measured_at: measurement.measured_at,
    };
    Ok((StatusCode::CREATED, Json(response)))
}
