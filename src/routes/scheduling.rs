//! Automatic payload-aware delivery scheduling.
//!
//! Generated deliveries are planning records. The existing mission validator still gates
//! every transition to dispatched or in-flight.

use std::time::Duration;

#[cfg(test)]
use crate::db::PgPool;
use chrono::Utc;
use tokio::{task::JoinHandle, time::MissedTickBehavior};

use crate::{error::AppError, state::AppState};

mod persistence;
mod planner;
mod repository;

use planner::valid_point;
#[allow(unused_imports)]
pub use planner::{
    DeliveryPlan, DroneCapacity, PlannedStop, ScheduleError, ScheduleOrder, plan_batches,
};

const SCHEDULER_INTERVAL: Duration = Duration::from_secs(5);
const MAX_CANDIDATES_PER_TICK: i64 = 200;
const DRONE_TURNAROUND_SECONDS: i64 = 600;
#[allow(dead_code)]
const DEFAULT_TELEMETRY_ONLINE_TTL_SECONDS: i64 = 120;

/// Starts the periodic scheduler and returns its task handle for application lifetime tracking.
pub fn spawn_scheduler(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SCHEDULER_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = run_once_with_ttl(&state, state.telemetry_online_ttl_seconds).await
            {
                tracing::error!(error = ?error, "automatic delivery scheduling failed");
            }
        }
    })
}

/// Assigns eligible orders once, with a transaction lock protecting concurrent instances.
#[cfg(test)]
pub async fn run_once(pool: &PgPool) -> Result<usize, AppError> {
    run_once_with_ttl(
        &crate::state::test_state(pool.clone()),
        DEFAULT_TELEMETRY_ONLINE_TTL_SECONDS as u64,
    )
    .await
}

async fn run_once_with_ttl(
    state: &AppState,
    telemetry_online_ttl_seconds: u64,
) -> Result<usize, AppError> {
    let pool = &state.pool;
    crate::routes::tracking::record_arrival_events(pool).await?;
    let zones = crate::routes::route_plans::load_active_zones(pool).await?;

    let mut connection = pool.get().await?;
    let mut tx = connection.transaction().await?;
    crate::db::execute(&tx, "SELECT pg_advisory_xact_lock(913720019, 21)", &[]).await?;

    let synchronized = repository::synchronize_assigned_orders(&mut tx).await?;
    let candidates = repository::lock_candidates(&mut tx, MAX_CANDIDATES_PER_TICK).await?;
    if candidates.is_empty() {
        tx.commit().await?;
        drop(connection);
        if synchronized > 0 {
            crate::routes::orders::invalidate_order_cache(state).await;
        }
        return Ok(0);
    }
    let telemetry_online_ttl_seconds =
        i64::try_from(telemetry_online_ttl_seconds).map_err(|_| {
            AppError::ServiceUnavailable("telemetry freshness setting is out of range".into())
        })?;
    let fleet = repository::load_active_fleet(
        &mut tx,
        DRONE_TURNAROUND_SECONDS,
        telemetry_online_ttl_seconds,
    )
    .await?;

    let mut orders = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Some((latitude, longitude)) = candidate.latitude.zip(candidate.longitude) else {
            persistence::add_blocked_event(&mut tx, candidate.id, "destination_missing").await?;
            continue;
        };
        if !valid_point(latitude, longitude) {
            persistence::add_blocked_event(&mut tx, candidate.id, "invalid_destination").await?;
            continue;
        }
        if !candidate.complete_weights {
            persistence::add_blocked_event(&mut tx, candidate.id, "product_weight_missing").await?;
            continue;
        }
        let Some(weight_grams) = candidate.estimated_weight_grams else {
            persistence::add_blocked_event(&mut tx, candidate.id, "product_weight_missing").await?;
            continue;
        };
        if weight_grams <= 0 {
            persistence::add_blocked_event(&mut tx, candidate.id, "invalid_estimated_weight")
                .await?;
            continue;
        }
        if fleet.is_empty() {
            persistence::add_blocked_event(&mut tx, candidate.id, "no_active_drone").await?;
            continue;
        }
        if fleet
            .iter()
            .all(|drone| drone.capacity_grams < weight_grams)
        {
            persistence::add_blocked_event(&mut tx, candidate.id, "payload_exceeds_fleet_capacity")
                .await?;
            continue;
        }
        orders.push(ScheduleOrder {
            id: candidate.id,
            latitude,
            longitude,
            weight_grams,
            created_at: candidate.created_at,
        });
    }

    let plans = plan_batches(&orders, &fleet, Utc::now()).map_err(|error| {
        AppError::ServiceUnavailable(format!("delivery plan could not be calculated: {error}"))
    })?;
    let mut assigned = 0;
    for plan in plans {
        persistence::persist_plan(&mut tx, &plan, state, &zones).await?;
        assigned += plan.stops.len();
    }

    tx.commit().await?;
    drop(connection);
    if assigned > 0 || synchronized > 0 {
        crate::routes::orders::invalidate_order_cache(state).await;
    }
    Ok(assigned)
}
