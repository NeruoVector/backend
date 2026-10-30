use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct Order {
    pub id: Uuid,
    pub order_number: String,
    pub customer_name: String,
    pub customer_phone: String,
    pub status: String,
    pub total_amount: i64,
    pub currency: String,
    pub delivery_address: String,
    pub delivery_detail: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub notes: Option<String>,
    pub delivery_id: Option<Uuid>,
    pub stock_restored_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
crate::db::row!(Order {
    id,
    order_number,
    customer_name,
    customer_phone,
    status,
    total_amount,
    currency,
    delivery_address,
    delivery_detail,
    latitude,
    longitude,
    notes,
    delivery_id,
    stock_restored_at,
    created_at,
    updated_at
});

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct OrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub item_id: Uuid,
    pub quantity: i32,
    pub unit_price: i64,
    pub line_total: i64,
    pub weight_grams: Option<i32>,
    pub created_at: DateTime<Utc>,
}
crate::db::row!(OrderItem {
    id,
    order_id,
    item_id,
    quantity,
    unit_price,
    line_total,
    weight_grams,
    created_at
});

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct Payment {
    pub id: Uuid,
    pub order_id: Uuid,
    pub provider: String,
    pub method: String,
    pub status: String,
    pub amount: i64,
    pub currency: String,
    pub transaction_id: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
crate::db::row!(Payment {
    id,
    order_id,
    provider,
    method,
    status,
    amount,
    currency,
    transaction_id,
    failure_reason,
    created_at,
    updated_at
});

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct OrderDetail {
    #[serde(flatten)]
    pub order: Order,
    pub items: Vec<OrderItem>,
    pub payment: Option<Payment>,
}

#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct CreateOrder {
    pub customer_name: String,
    pub customer_phone: String,
    pub items: Vec<CreateOrderItem>,
    pub delivery_address: String,
    #[serde(default)]
    pub delivery_detail: Option<String>,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub delivery_id: Option<Uuid>,
    #[serde(default)]
    pub spot_id: Option<Uuid>,
    #[serde(default)]
    pub room_number: Option<String>,
    #[serde(default)]
    pub grade: Option<i32>,
    #[serde(default)]
    pub classroom: Option<String>,
    #[serde(default, skip_serializing)]
    pub handoff_pin: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct CreateOrderItem {
    pub item_id: Uuid,
    pub quantity: i32,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListOrdersParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateOrderStatus {
    pub status: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePayment {
    #[serde(default)]
    pub method: Option<String>,
    /// `success` 또는 `failure`. 더미 결제 시뮬레이션에 사용합니다.
    #[serde(default)]
    pub simulate: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StockItem {
    pub id: Uuid,
    pub name: String,
    pub unit_price: i64,
    pub currency: String,
    pub stock_quantity: i32,
    pub active: bool,
    pub merchant_id: Option<Uuid>,
    pub weight_grams: Option<i32>,
}
crate::db::row!(StockItem {
    id,
    name,
    unit_price,
    currency,
    stock_quantity,
    active,
    merchant_id,
    weight_grams
});
