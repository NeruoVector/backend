use deadpool_postgres::Transaction;
use uuid::Uuid;

use crate::error::AppError;
use crate::routes::orders::Order;

pub(crate) struct CustomerOrderContext {
    pub customer_id: Uuid,
    pub spot_id: Option<Uuid>,
}

struct SpotSnapshot {
    delivery_address: String,
    delivery_detail: Option<String>,
    latitude: f64,
    longitude: f64,
    room_number: Option<String>,
    grade: Option<i32>,
    classroom: Option<String>,
}
crate::db::row!(SpotSnapshot {
    delivery_address,
    delivery_detail,
    latitude,
    longitude,
    room_number,
    grade,
    classroom
});

pub(crate) async fn attach(
    tx: &mut Transaction<'_>,
    order_id: Uuid,
    context: CustomerOrderContext,
) -> Result<Order, AppError> {
    let Some(spot_id) = context.spot_id else {
        let order = crate::db::fetch_one::<Order>(
            &*tx,
            r#"
            UPDATE neurovector.orders SET customer_id = $2 WHERE id = $1
            RETURNING id, order_number, customer_name, customer_phone, status, total_amount,
                      currency, delivery_address, delivery_detail, latitude, longitude,
                      notes, delivery_id, stock_restored_at, created_at, updated_at
            "#,
            &[&(order_id), &(context.customer_id)],
        )
        .await?;
        return Ok(order);
    };
    let spot = crate::db::fetch_optional::<SpotSnapshot>(
        &*tx,
        r#"
        SELECT delivery_address, delivery_detail, latitude, longitude,
               room_number, grade, classroom
        FROM neurovector.delivery_spots
        WHERE id = $1 AND customer_id = $2
        FOR SHARE
        "#,
        &[&(spot_id), &(context.customer_id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    let order = crate::db::fetch_one::<Order>(
        &*tx,
        r#"
        UPDATE neurovector.orders
        SET customer_id = $2, spot_id = $3, delivery_address = $4,
            delivery_detail = $5, latitude = $6, longitude = $7,
            room_number = $8, grade = $9, classroom = $10
        WHERE id = $1
        RETURNING id, order_number, customer_name, customer_phone, status, total_amount,
                  currency, delivery_address, delivery_detail, latitude, longitude,
                  notes, delivery_id, stock_restored_at, created_at, updated_at
        "#,
        &[
            &(order_id),
            &(context.customer_id),
            &(spot_id),
            &(spot.delivery_address),
            &(spot.delivery_detail),
            &(spot.latitude),
            &(spot.longitude),
            &(spot.room_number),
            &(spot.grade),
            &(spot.classroom),
        ],
    )
    .await?;
    Ok(order)
}
