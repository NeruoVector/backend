use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct FleetDrone {
    pub drone_id: String,
    pub capacity_grams: i32,
    pub home_latitude: f64,
    pub home_longitude: f64,
    pub cruise_speed_mps: f64,
    pub max_altitude_m: f64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
crate::db::row!(FleetDrone {
    drone_id,
    capacity_grams,
    home_latitude,
    home_longitude,
    cruise_speed_mps,
    max_altitude_m,
    active,
    created_at,
    updated_at
});

#[derive(Debug, Deserialize)]
pub struct FleetDroneInput {
    pub drone_id: String,
    pub capacity_grams: i32,
    pub home_latitude: f64,
    pub home_longitude: f64,
    pub cruise_speed_mps: f64,
    #[serde(default = "default_max_altitude")]
    pub max_altitude_m: f64,
    #[serde(default = "default_active")]
    pub active: bool,
}

fn default_max_altitude() -> f64 {
    30.0
}

fn default_active() -> bool {
    true
}

impl FleetDroneInput {
    pub(super) fn validate(&mut self) -> Result<(), crate::error::AppError> {
        self.drone_id = self.drone_id.trim().to_owned();
        if self.drone_id.is_empty()
            || self.drone_id.len() > 64
            || !self
                .drone_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(crate::error::AppError::BadRequest(
                "drone_id may contain only letters, digits, dot, underscore, and hyphen".into(),
            ));
        }
        if self.capacity_grams <= 0 {
            return Err(crate::error::AppError::BadRequest(
                "capacity_grams must be positive".into(),
            ));
        }
        if !self.home_latitude.is_finite() || !(-90.0..=90.0).contains(&self.home_latitude) {
            return Err(crate::error::AppError::BadRequest(
                "home_latitude must be finite and between -90 and 90".into(),
            ));
        }
        if !self.home_longitude.is_finite() || !(-180.0..=180.0).contains(&self.home_longitude) {
            return Err(crate::error::AppError::BadRequest(
                "home_longitude must be finite and between -180 and 180".into(),
            ));
        }
        if !self.cruise_speed_mps.is_finite() || !(0.1..=100.0).contains(&self.cruise_speed_mps) {
            return Err(crate::error::AppError::BadRequest(
                "cruise_speed_mps must be between 0.1 and 100".into(),
            ));
        }
        if !self.max_altitude_m.is_finite() || !(1.0..=120.0).contains(&self.max_altitude_m) {
            return Err(crate::error::AppError::BadRequest(
                "max_altitude_m must be between 1 and 120".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct PayloadMeasurementInput {
    pub delivery_id: Uuid,
    pub weight_grams: i32,
}

#[derive(Debug, Serialize)]
pub struct PayloadMeasurement {
    pub id: Uuid,
    pub delivery_id: Uuid,
    pub drone_id: String,
    pub estimated_delivery_weight_grams: Option<i64>,
    pub weight_grams: i32,
    pub capacity_grams: i32,
    pub capacity_exceeded: bool,
    pub measured_by: String,
    pub measured_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> FleetDroneInput {
        FleetDroneInput {
            drone_id: " drone-1 ".into(),
            capacity_grams: 1_000,
            home_latitude: 37.5,
            home_longitude: 127.0,
            cruise_speed_mps: 8.0,
            max_altitude_m: 30.0,
            active: true,
        }
    }

    #[test]
    fn fleet_configuration_accepts_finite_bounded_values() {
        // Given a valid drone configuration, When validating, Then the normalized ID and bounded limits are accepted.
        let mut config = input();
        assert!(config.validate().is_ok());
        assert_eq!(config.drone_id, "drone-1");
    }

    #[test]
    fn fleet_configuration_rejects_invalid_coordinates_and_capacity() {
        // Given non-finite home coordinates or zero capacity, When validating, Then each configuration is rejected.
        let mut coordinates = input();
        coordinates.home_latitude = f64::INFINITY;
        assert!(coordinates.validate().is_err());

        let mut capacity = input();
        capacity.capacity_grams = 0;
        assert!(capacity.validate().is_err());
    }
}
