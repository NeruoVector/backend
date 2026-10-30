use std::collections::{HashMap, HashSet};

use crate::error::AppError;

use super::models::CreateOrder;

pub(crate) fn validate_create_order(body: &CreateOrder) -> Result<(), AppError> {
    if let Some(pin) = body.handoff_pin.as_deref() {
        crate::handoff::validate_pin(pin)?;
    }
    if body.spot_id.is_none()
        || body.room_number.is_some()
        || body.grade.is_some()
        || body.classroom.is_some()
    {
        validate_indoor_destination(
            body.room_number.as_deref(),
            body.grade,
            body.classroom.as_deref(),
        )?;
    }
    if body.customer_name.trim().is_empty() {
        return Err(AppError::BadRequest("customer_name is required".into()));
    }
    if body.customer_phone.trim().is_empty() {
        return Err(AppError::BadRequest("customer_phone is required".into()));
    }
    if body.delivery_address.trim().is_empty() {
        return Err(AppError::BadRequest("delivery_address is required".into()));
    }
    if body.items.is_empty() {
        return Err(AppError::BadRequest(
            "at least one order item is required".into(),
        ));
    }
    if body.latitude.is_some() != body.longitude.is_some() {
        return Err(AppError::BadRequest(
            "latitude and longitude must be provided together".into(),
        ));
    }
    if let Some(lat) = body.latitude
        && !(-90.0..=90.0).contains(&lat)
    {
        return Err(AppError::BadRequest(
            "latitude must be between -90 and 90".into(),
        ));
    }
    if let Some(lon) = body.longitude
        && !(-180.0..=180.0).contains(&lon)
    {
        return Err(AppError::BadRequest(
            "longitude must be between -180 and 180".into(),
        ));
    }

    let mut seen = HashSet::new();
    for item in &body.items {
        if item.quantity <= 0 || item.quantity > 100 {
            return Err(AppError::BadRequest(
                "item quantity must be between 1 and 100".into(),
            ));
        }
        if !seen.insert(item.item_id) {
            return Err(AppError::BadRequest(
                "duplicate item_id in order items".into(),
            ));
        }
    }

    Ok(())
}

fn validate_indoor_destination(
    room_number: Option<&str>,
    grade: Option<i32>,
    classroom: Option<&str>,
) -> Result<(), AppError> {
    let room = room_number.map(str::trim);
    let class = classroom.map(str::trim);
    match (room, grade, class) {
        (None, None, None) => Ok(()),
        (Some(room), None, None) if !room.is_empty() && room.chars().count() <= 40 => Ok(()),
        (None, Some(grade), Some(class))
            if (1..=12).contains(&grade) && !class.is_empty() && class.chars().count() <= 40 =>
        {
            Ok(())
        }
        _ => Err(AppError::BadRequest(
            "provide either room_number or both grade and classroom".into(),
        )),
    }
}

pub(super) fn validate_order_transition(current: &str, next: &str) -> Result<(), AppError> {
    let allowed: HashMap<&str, &[&str]> = HashMap::from([
        ("pending_payment", &["paid", "cancelled"][..]),
        ("paid", &["preparing", "cancelled"][..]),
        ("preparing", &["ready", "cancelled"][..]),
        ("ready", &["assigned", "cancelled"][..]),
        ("assigned", &["in_transit", "cancelled"][..]),
        ("in_transit", &["delivered", "failed"][..]),
        ("delivered", &[][..]),
        ("failed", &[][..]),
        ("cancelled", &[][..]),
    ]);

    if current == next {
        return Err(AppError::Conflict(format!(
            "order is already in status {current}"
        )));
    }
    let Some(allowed_next) = allowed.get(current) else {
        return Err(AppError::BadRequest(format!(
            "unknown current status {current}"
        )));
    };
    if !allowed_next.contains(&next) {
        return Err(AppError::Conflict(format!(
            "cannot transition order from {current} to {next}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::orders::models::CreateOrderItem;
    use uuid::Uuid;

    fn valid_order() -> CreateOrder {
        CreateOrder {
            customer_name: "Test User".into(),
            customer_phone: "010-0000-0000".into(),
            items: vec![CreateOrderItem {
                item_id: Uuid::new_v4(),
                quantity: 1,
            }],
            delivery_address: "Seoul".into(),
            delivery_detail: None,
            latitude: Some(37.5),
            longitude: Some(127.0),
            notes: None,
            delivery_id: None,
            spot_id: None,
            room_number: None,
            grade: None,
            classroom: None,
            handoff_pin: None,
        }
    }

    #[test]
    fn accepts_valid_order() {
        assert!(validate_create_order(&valid_order()).is_ok());
    }

    #[test]
    fn order_serialization_omits_the_handoff_pin() {
        let mut order = valid_order();
        order.handoff_pin = Some("042731".into());
        let payload = serde_json::to_value(order).expect("serialize order request");
        assert!(payload.get("handoff_pin").is_none());
    }

    #[test]
    fn rejects_duplicate_items_and_bad_coordinates() {
        let mut order = valid_order();
        order.items.push(order.items[0].clone());
        assert!(validate_create_order(&order).is_err());

        let mut order = valid_order();
        order.latitude = Some(120.0);
        assert!(validate_create_order(&order).is_err());

        let mut order = valid_order();
        order.latitude = Some(37.5);
        order.longitude = None;
        assert!(validate_create_order(&order).is_err());
    }
}
