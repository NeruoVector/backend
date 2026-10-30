use crate::db::PgPool;
use axum::http::HeaderMap;
use deadpool_postgres::Transaction;
use uuid::Uuid;

use crate::{customer_auth, error::AppError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Actor {
    Admin,
    Customer(Uuid),
    Anonymous,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OrderAccess {
    pub actor: Actor,
}

impl OrderAccess {
    pub fn cache_scope(self) -> String {
        match self.actor {
            Actor::Admin => "admin".into(),
            Actor::Customer(id) => format!("customer:{id}"),
            Actor::Anonymous => "legacy-guest".into(),
        }
    }
}

pub(super) async fn actor(pool: &PgPool, headers: &HeaderMap) -> Result<Actor, AppError> {
    if let Some(token) = customer_auth::cookie_value(headers, crate::auth::SESSION_COOKIE_NAME) {
        let valid = crate::db::scalar_one::<bool>(&pool.get().await?, "SELECT EXISTS (SELECT 1 FROM neurovector.admin_sessions WHERE session_hash = $1 AND expires_at > now())", &[&(customer_auth::digest(&token))])
        .await?;
        if valid {
            return Ok(Actor::Admin);
        }
    }

    if let Some(token) = customer_auth::cookie_value(headers, "nv_customer") {
        let customer_id = crate::db::scalar_optional::<Uuid>(&pool.get().await?, "SELECT customer_id FROM neurovector.customer_sessions WHERE token_hash = $1 AND expires_at > now()", &[&(customer_auth::digest(&token))])
        .await?;
        if let Some(id) = customer_id {
            return Ok(Actor::Customer(id));
        }
    }
    Ok(Actor::Anonymous)
}

pub(super) async fn authorize_order(
    pool: &PgPool,
    headers: &HeaderMap,
    order_id: Uuid,
) -> Result<OrderAccess, AppError> {
    let owner_id = crate::db::scalar_optional::<Option<Uuid>>(
        &pool.get().await?,
        "SELECT customer_id FROM neurovector.orders WHERE id = $1",
        &[&(order_id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    let actor = actor(pool, headers).await?;

    match (owner_id, actor) {
        (Some(owner), Actor::Customer(customer)) if owner == customer => Ok(OrderAccess { actor }),
        (Some(_), Actor::Admin) => Ok(OrderAccess { actor }),
        (Some(_), Actor::Customer(_)) => Err(AppError::NotFound),
        (Some(_), Actor::Anonymous) => Err(AppError::Unauthorized(
            "sign in to access this order".into(),
        )),
        (None, _) => Ok(OrderAccess { actor }),
    }
}

pub(super) async fn is_merchant_owner(
    tx: &mut Transaction<'_>,
    order_id: Uuid,
    customer_id: Uuid,
) -> Result<bool, AppError> {
    Ok(crate::db::scalar_one::<bool>(
        &*tx,
        r#"
        SELECT COUNT(*) > 0
           AND COUNT(*) = COUNT(details.item_id)
           AND COALESCE(bool_and(details.merchant_id = merchant.id), false)
        FROM neurovector.order_items items
        LEFT JOIN neurovector.product_details details ON details.item_id = items.item_id
        JOIN neurovector.merchants merchant ON merchant.owner_id = $2
        WHERE items.order_id = $1
        "#,
        &[&(order_id), &(customer_id)],
    )
    .await?)
}

pub(super) fn customer_can_cancel(
    actor: Actor,
    owner_id: Option<Uuid>,
    current_status: &str,
    next_status: &str,
) -> bool {
    matches!(actor, Actor::Customer(customer) if Some(customer) == owner_id)
        && current_status == "pending_payment"
        && next_status == "cancelled"
}

pub(super) fn merchant_can_transition(current: &str, next: &str) -> bool {
    matches!(
        (current, next),
        ("paid", "preparing" | "cancelled")
            | ("preparing", "ready" | "cancelled")
            | ("ready", "cancelled")
    )
}

#[cfg(test)]
mod tests {
    use super::{Actor, customer_can_cancel, merchant_can_transition};
    use uuid::Uuid;

    #[test]
    fn customer_can_only_be_the_owner_for_cancellation() {
        let owner = Uuid::new_v4();
        assert!(customer_can_cancel(
            Actor::Customer(owner),
            Some(owner),
            "pending_payment",
            "cancelled"
        ));
        assert!(!customer_can_cancel(
            Actor::Customer(Uuid::new_v4()),
            Some(owner),
            "pending_payment",
            "cancelled"
        ));
        assert!(!customer_can_cancel(
            Actor::Customer(owner),
            Some(owner),
            "paid",
            "cancelled"
        ));
    }

    #[test]
    fn merchant_transition_set_excludes_paid_and_delivery_forgery() {
        assert!(merchant_can_transition("paid", "preparing"));
        assert!(merchant_can_transition("preparing", "ready"));
        assert!(!merchant_can_transition("paid", "delivered"));
        assert!(!merchant_can_transition("ready", "assigned"));
    }
}
