use deadpool_postgres::Transaction;
use uuid::Uuid;

use crate::error::AppError;

use super::planner::DeliveryPlan;

pub(super) async fn add_blocked_event(
    tx: &mut Transaction<'_>,
    order_id: Uuid,
    reason: &str,
) -> Result<(), AppError> {
    let dedupe_key = format!("schedule_blocked:{reason}");
    crate::db::execute(
        &*tx,
        r#"
        INSERT INTO neurovector.order_tracking_events
            (order_id, event_type, dedupe_key, payload)
        VALUES ($1, 'schedule_blocked', $2, jsonb_build_object('reason', $3::text))
        ON CONFLICT (order_id, dedupe_key) DO NOTHING
        "#,
        &[&(order_id), &(dedupe_key), &(reason)],
    )
    .await?;
    Ok(())
}

pub(super) async fn persist_plan(
    tx: &mut Transaction<'_>,
    plan: &DeliveryPlan,
    state: &crate::state::AppState,
    zones: &[crate::routes::route_plans::NoFlyZoneRow],
) -> Result<(), AppError> {
    let fleet = crate::db::fetch_one::<(f64, f64)>(&*tx, "SELECT cruise_speed_mps, max_altitude_m FROM neurovector.delivery_fleet WHERE drone_id=$1 AND active", &[&plan.drone_id]).await?;
    let input = crate::routes::route_plans::RouteInput {
        name: format!("automatic-delivery-{}", Uuid::new_v4()),
        drone_id: Some(plan.drone_id.clone()),
        waypoints: plan.waypoints.clone(),
        cruise_speed_mps: Some(fleet.0),
        max_altitude_m: Some(fleet.1),
    };
    let payload = crate::mission::automatic_payload(state, input, zones.to_vec())?;
    let selected = &payload.candidates[payload.selected_index];
    let validation = &payload.validations[payload.selected_index];
    let waypoints = serde_json::to_value(&selected.waypoints).map_err(anyhow::Error::from)?;
    let route_id = crate::db::scalar_optional::<Uuid>(
        &*tx,
        r#"
        INSERT INTO neurovector.routes
            (name, drone_id, waypoints, cruise_speed_mps, max_altitude_m,
             distance_m, estimated_duration_s)
        SELECT $1, $2, $3, f.cruise_speed_mps, f.max_altitude_m, $4, $5
        FROM neurovector.delivery_fleet f
        WHERE f.drone_id = $2 AND f.active
        RETURNING id
        "#,
        &[
            &(&selected.name),
            &(&plan.drone_id),
            &(waypoints),
            &(validation.distance_m),
            &(validation.estimated_duration_s),
        ],
    )
    .await?
    .ok_or_else(|| {
        AppError::Conflict("drone fleet registration changed during scheduling".into())
    })?;

    crate::mission::persist_pending_plan(tx, route_id, &plan.drone_id, &payload).await?;

    let delivery_id = crate::db::scalar_one::<Uuid>(
        &*tx,
        r#"
        INSERT INTO neurovector.deliveries
            (route_id, drone_id, cargo, status, scheduled_at, notes,
             estimated_weight_grams, schedule_source)
        VALUES ($1, $2, $3, 'scheduled', $4,
                'planned automatically; mission validation is required before dispatch',
                $5, 'automatic')
        RETURNING id
        "#,
        &[
            &(route_id),
            &(&plan.drone_id),
            &(format!("{} scheduled customer orders", plan.stops.len())),
            &(plan.scheduled_at),
            &(plan.estimated_weight_grams),
        ],
    )
    .await?;

    let order_ids = plan
        .stops
        .iter()
        .map(|stop| stop.order_id)
        .collect::<Vec<_>>();
    let sequences = plan
        .stops
        .iter()
        .map(|stop| stop.sequence)
        .collect::<Vec<_>>();
    let weights = plan
        .stops
        .iter()
        .map(|stop| {
            i32::try_from(stop.weight_grams)
                .map_err(|_| AppError::Conflict("planned stop weight exceeds storage range".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let etas = plan
        .stops
        .iter()
        .map(|stop| stop.eta_at)
        .collect::<Vec<_>>();
    if order_ids.len() != sequences.len()
        || order_ids.len() != weights.len()
        || order_ids.len() != etas.len()
    {
        return Err(AppError::Conflict(
            "planned stop arrays have different lengths".into(),
        ));
    }

    crate::db::execute(
        &*tx,
        r#"
        INSERT INTO neurovector.delivery_stops
            (delivery_id, order_id, stop_sequence, estimated_weight_grams, eta_at)
        SELECT $1, stop.order_id, stop.stop_sequence, stop.weight_grams, stop.eta_at
        FROM unnest($2::uuid[], $3::integer[], $4::integer[], $5::timestamptz[])
             AS stop(order_id, stop_sequence, weight_grams, eta_at)
        "#,
        &[
            &(delivery_id),
            &(&order_ids),
            &(&sequences),
            &(&weights),
            &(&etas),
        ],
    )
    .await?;

    let updated = crate::db::execute(
        &*tx,
        r#"
        UPDATE neurovector.orders o
        SET delivery_id = $1,
            status = CASE WHEN o.status = 'ready' THEN 'assigned' ELSE o.status END,
            updated_at = now()
        FROM unnest($2::uuid[]) AS assigned(order_id)
        WHERE o.id = assigned.order_id
          AND o.delivery_id IS NULL AND o.status IN ('paid', 'ready')
        "#,
        &[&(delivery_id), &(&order_ids)],
    )
    .await?;
    if updated != plan.stops.len() as u64 {
        return Err(AppError::Conflict(
            "order changed while its delivery was being assigned".into(),
        ));
    }

    crate::db::execute(
        &*tx,
        r#"
        INSERT INTO neurovector.order_tracking_events
            (order_id, delivery_stop_id, event_type, dedupe_key, payload, occurred_at)
        SELECT s.order_id, s.id, 'delivery_assigned',
               'delivery_assigned:' || s.delivery_id::text,
               jsonb_build_object(
                   'delivery_id', s.delivery_id,
                   'drone_id', $2::text,
                   'eta_at', s.eta_at,
                   'estimated_weight_grams', s.estimated_weight_grams,
                   'estimate_only', true
               ),
               now()
        FROM neurovector.delivery_stops s
        WHERE s.delivery_id = $1
        ON CONFLICT (order_id, dedupe_key) DO NOTHING
        "#,
        &[&(delivery_id), &(&plan.drone_id)],
    )
    .await?;

    tracing::info!(
        delivery_id = %delivery_id,
        drone_id = %plan.drone_id,
        stop_count = plan.stops.len(),
        estimated_weight_grams = plan.estimated_weight_grams,
        "automatically planned a delivery batch"
    );
    Ok(())
}
