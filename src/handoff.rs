use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use chrono::{DateTime, Utc};
use deadpool_postgres::Transaction;
use uuid::Uuid;

use crate::error::AppError;

mod api;

const MAX_PIN_ATTEMPTS: i32 = 5;
static PIN_PASSWORD_WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub(crate) struct NewOrderHandoff {
    pub customer_id: Uuid,
    pub pin_hash: String,
}

struct HandoffRecord {
    customer_id: Uuid,
    pin_hash: String,
    attempts: i32,
    consumed_at: Option<DateTime<Utc>>,
}
crate::db::row!(HandoffRecord {
    customer_id,
    pin_hash,
    attempts,
    consumed_at
});

pub fn router(state: crate::state::AppState) -> axum::Router<crate::state::AppState> {
    api::router(state)
}

pub(crate) fn validate_pin(pin: &str) -> Result<(), AppError> {
    if pin.len() != 6 || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::BadRequest(
            "handoff_pin must be exactly six digits".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn prepare_order_handoff(
    customer_id: Uuid,
    pin: &str,
) -> Result<NewOrderHandoff, AppError> {
    validate_pin(pin)?;
    let pin = pin.to_owned();
    let permit = PIN_PASSWORD_WORKERS
        .acquire()
        .await
        .map_err(|_| AppError::ServiceUnavailable("PIN hashing unavailable".into()))?;
    let pin_hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(pin.as_bytes(), &salt)
            .map(|hash| hash.to_string())
    })
    .await
    .map_err(|_| AppError::ServiceUnavailable("PIN hashing failed".into()))?
    .map_err(|_| AppError::ServiceUnavailable("PIN hashing failed".into()))?;
    Ok(NewOrderHandoff {
        customer_id,
        pin_hash,
    })
}

pub(crate) async fn store_order_handoff(
    tx: &mut Transaction<'_>,
    order_id: Uuid,
    handoff: &NewOrderHandoff,
) -> Result<(), AppError> {
    crate::db::execute(&*tx, "INSERT INTO neurovector.order_handoffs (order_id, customer_id, pin_hash) VALUES ($1, $2, $3)", &[&(order_id), &(handoff.customer_id), &(&handoff.pin_hash)])
    .await?;
    Ok(())
}

async fn verify_pin(hash: String, pin: String) -> Result<bool, AppError> {
    let permit = PIN_PASSWORD_WORKERS
        .acquire()
        .await
        .map_err(|_| AppError::ServiceUnavailable("PIN verification unavailable".into()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(pin.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .map_err(|_| AppError::ServiceUnavailable("PIN verification failed".into()))
}

fn handoff_allowed(order_status: &str, stop_status: Option<&str>) -> bool {
    order_status == "in_transit"
        && match stop_status {
            Some(status) => status == "arrived",
            None => false,
        }
}

#[cfg(test)]
mod tests {
    use super::{handoff_allowed, prepare_order_handoff, validate_pin, verify_pin};
    use uuid::Uuid;

    #[test]
    fn validate_pin_accepts_six_digits_when_ascii() {
        assert!(validate_pin("042731").is_ok());
    }

    #[test]
    fn validate_pin_rejects_wrong_length() {
        assert!(validate_pin("12345").is_err());
        assert!(validate_pin("1234567").is_err());
    }

    #[test]
    fn validate_pin_rejects_non_ascii_digits() {
        assert!(validate_pin("１２３４５６").is_err());
        assert!(validate_pin("12a456").is_err());
    }

    #[test]
    fn handoff_requires_transit_and_stop_arrival() {
        assert!(handoff_allowed("in_transit", Some("arrived")));
        assert!(!handoff_allowed("in_transit", None));
        assert!(!handoff_allowed("in_transit", Some("scheduled")));
        assert!(!handoff_allowed("assigned", Some("arrived")));
    }

    #[tokio::test]
    async fn prepare_handoff_stores_a_password_hash() {
        let handoff = prepare_order_handoff(Uuid::new_v4(), "042731")
            .await
            .expect("valid PIN should be hashed");
        assert_ne!(handoff.pin_hash, "042731");
    }

    #[tokio::test]
    async fn verify_pin_accepts_and_rejects_as_expected() {
        let handoff = prepare_order_handoff(Uuid::new_v4(), "042731")
            .await
            .expect("valid PIN should be hashed");
        assert!(
            verify_pin(handoff.pin_hash.clone(), "042731".into())
                .await
                .unwrap()
        );
        assert!(!verify_pin(handoff.pin_hash, "042732".into()).await.unwrap());
    }
}
