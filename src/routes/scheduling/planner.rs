use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::geo::{self, Waypoint};

const MAX_ORDERS_PER_DELIVERY: usize = 8;

#[derive(Debug, Clone)]
pub struct ScheduleOrder {
    pub id: Uuid,
    pub latitude: f64,
    pub longitude: f64,
    pub weight_grams: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct DroneCapacity {
    pub drone_id: String,
    pub capacity_grams: i64,
    pub home_latitude: f64,
    pub home_longitude: f64,
    pub cruise_speed_mps: f64,
    pub max_altitude_m: f64,
    pub available_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedStop {
    pub order_id: Uuid,
    pub eta_at: DateTime<Utc>,
    pub sequence: i32,
    #[serde(skip)]
    pub weight_grams: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliveryPlan {
    pub drone_id: String,
    pub scheduled_at: DateTime<Utc>,
    pub stops: Vec<PlannedStop>,
    pub waypoints: Vec<Waypoint>,
    pub distance_m: f64,
    pub estimated_duration_s: f64,
    pub estimated_weight_grams: i64,
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error("combined merchandise weight overflowed")]
    WeightOverflow,
    #[error("estimated route duration is outside the supported range")]
    InvalidDuration,
}

/// Packs oldest eligible orders into one multi-stop run per drone, respecting payload limits.
pub fn plan_batches(
    candidates: &[ScheduleOrder],
    fleet: &[DroneCapacity],
    now: DateTime<Utc>,
) -> Result<Vec<DeliveryPlan>, ScheduleError> {
    let mut orders = candidates.to_vec();
    orders.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut drones = fleet.to_vec();
    drones.sort_by(|left, right| {
        left.available_at
            .cmp(&right.available_at)
            .then_with(|| left.drone_id.cmp(&right.drone_id))
    });

    let mut plans = Vec::new();
    for drone in drones {
        if drone.capacity_grams <= 0
            || !drone.cruise_speed_mps.is_finite()
            || drone.cruise_speed_mps <= 0.0
            || !valid_point(drone.home_latitude, drone.home_longitude)
        {
            continue;
        }
        let first = orders.iter().position(|order| {
            order.weight_grams > 0
                && order.weight_grams <= drone.capacity_grams
                && valid_point(order.latitude, order.longitude)
        });
        let Some(first) = first else {
            continue;
        };
        let mut selected = vec![orders.remove(first)];
        let mut total_weight = selected[0].weight_grams;
        let mut current = (selected[0].latitude, selected[0].longitude);

        while selected.len() < MAX_ORDERS_PER_DELIVERY {
            let remaining_capacity = drone
                .capacity_grams
                .checked_sub(total_weight)
                .ok_or(ScheduleError::WeightOverflow)?;
            let next = orders
                .iter()
                .enumerate()
                .filter(|(_, order)| {
                    order.weight_grams > 0
                        && order.weight_grams <= remaining_capacity
                        && valid_point(order.latitude, order.longitude)
                })
                .min_by(|(_, left), (_, right)| {
                    let left_distance = geo::haversine_m(current, (left.latitude, left.longitude));
                    let right_distance =
                        geo::haversine_m(current, (right.latitude, right.longitude));
                    left_distance
                        .total_cmp(&right_distance)
                        .then_with(|| left.created_at.cmp(&right.created_at))
                        .then_with(|| left.id.cmp(&right.id))
                })
                .map(|(index, _)| index);
            let Some(index) = next else {
                break;
            };
            let order = orders.remove(index);
            total_weight = total_weight
                .checked_add(order.weight_grams)
                .ok_or(ScheduleError::WeightOverflow)?;
            current = (order.latitude, order.longitude);
            selected.push(order);
        }

        let scheduled_at = drone.available_at.max(now);
        let altitude_m = drone.max_altitude_m.min(120.0);
        let mut waypoints = Vec::with_capacity(selected.len() + 2);
        waypoints.push(Waypoint {
            lat: drone.home_latitude,
            lon: drone.home_longitude,
            altitude_m,
        });
        waypoints.extend(selected.iter().map(|order| Waypoint {
            lat: order.latitude,
            lon: order.longitude,
            altitude_m,
        }));
        waypoints.push(Waypoint {
            lat: drone.home_latitude,
            lon: drone.home_longitude,
            altitude_m,
        });

        let mut distance_m = 0.0;
        let mut previous = (drone.home_latitude, drone.home_longitude);
        let mut stops = Vec::with_capacity(selected.len());
        for (index, order) in selected.iter().enumerate() {
            distance_m += geo::haversine_m(previous, (order.latitude, order.longitude));
            let elapsed_s = distance_m / drone.cruise_speed_mps;
            let eta_at = add_seconds(scheduled_at, elapsed_s)?;
            let sequence = i32::try_from(index + 1).map_err(|_| ScheduleError::InvalidDuration)?;
            stops.push(PlannedStop {
                order_id: order.id,
                eta_at,
                sequence,
                weight_grams: order.weight_grams,
            });
            previous = (order.latitude, order.longitude);
        }
        distance_m += geo::haversine_m(previous, (drone.home_latitude, drone.home_longitude));
        let estimated_duration_s = distance_m / drone.cruise_speed_mps;
        if !estimated_duration_s.is_finite() {
            return Err(ScheduleError::InvalidDuration);
        }

        plans.push(DeliveryPlan {
            drone_id: drone.drone_id,
            scheduled_at,
            stops,
            waypoints,
            distance_m,
            estimated_duration_s,
            estimated_weight_grams: total_weight,
        });
    }

    Ok(plans)
}

pub(super) fn valid_point(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

fn add_seconds(start: DateTime<Utc>, seconds: f64) -> Result<DateTime<Utc>, ScheduleError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(ScheduleError::InvalidDuration);
    }
    let duration =
        Duration::try_from_secs_f64(seconds).map_err(|_| ScheduleError::InvalidDuration)?;
    let duration =
        ChronoDuration::from_std(duration).map_err(|_| ScheduleError::InvalidDuration)?;
    start
        .checked_add_signed(duration)
        .ok_or(ScheduleError::InvalidDuration)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drone(id: &str, capacity: i64, available_at: DateTime<Utc>) -> DroneCapacity {
        DroneCapacity {
            drone_id: id.to_string(),
            capacity_grams: capacity,
            home_latitude: 37.5,
            home_longitude: 127.0,
            cruise_speed_mps: 8.0,
            max_altitude_m: 30.0,
            available_at,
        }
    }

    fn order(
        id: Uuid,
        created_at: DateTime<Utc>,
        latitude_offset: f64,
        weight: i64,
    ) -> ScheduleOrder {
        ScheduleOrder {
            id,
            latitude: 37.5 + latitude_offset,
            longitude: 127.0,
            weight_grams: weight,
            created_at,
        }
    }

    #[test]
    fn planner_batches_multiple_orders_per_drone_without_exceeding_payload() {
        // Given nearby orders whose combined weight fits one drone, When planning, Then they share one multi-stop batch within capacity.
        let now = Utc::now();
        let candidates = vec![
            order(Uuid::from_u128(1), now, 0.001, 100),
            order(Uuid::from_u128(2), now, 0.002, 150),
            order(Uuid::from_u128(3), now, 0.003, 160),
        ];
        let plans =
            plan_batches(&candidates, &[drone("d1", 300, now)], now).expect("valid delivery plan");

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].stops.len(), 2);
        assert_eq!(plans[0].estimated_weight_grams, 250);
        assert!(plans[0].estimated_weight_grams <= 300);
    }

    #[test]
    fn planner_assigns_concurrent_batches_to_multiple_drones_and_leaves_overweight_orders_queued() {
        // Given two available drones plus an overweight order, When planning, Then both drones receive work and the overweight order stays queued.
        let now = Utc::now();
        let candidates = vec![
            order(Uuid::from_u128(1), now, 0.001, 100),
            order(Uuid::from_u128(2), now, 0.002, 100),
            order(Uuid::from_u128(3), now, 0.003, 100),
            order(Uuid::from_u128(4), now, 0.004, 600),
        ];
        let plans = plan_batches(
            &candidates,
            &[drone("d1", 200, now), drone("d2", 200, now)],
            now,
        )
        .expect("valid delivery plans");

        assert_eq!(plans.len(), 2);
        assert_eq!(plans.iter().map(|plan| plan.stops.len()).sum::<usize>(), 3);
        assert!(plans.iter().all(|plan| plan.estimated_weight_grams <= 200));
        assert!(plans.iter().all(|plan| plan.scheduled_at == now));
    }
}
