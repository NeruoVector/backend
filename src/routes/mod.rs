use axum::Router;

use crate::state::AppState;

pub mod admin;
pub mod customer_admin;
pub mod customer_login;
pub mod deliveries;
pub mod detections;
pub mod fleet;
pub mod flights;
pub mod inventory;
pub mod orders;
pub mod risk;
pub mod route_plans;
pub mod scheduling;
pub mod spots;
pub mod streams;
pub mod tracking;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .nest("/detections", detections::router())
        .nest("/routes", route_plans::router())
        .nest("/deliveries", deliveries::router())
        .nest("/flights", flights::router())
        .nest("/risk", risk::router())
        .nest("/inventory", inventory::router(state.clone()))
        .merge(fleet::router(state.clone()))
        .merge(spots::router().route_layer(axum::middleware::from_fn(
            crate::customer_auth::require_same_origin,
        )))
        .merge(tracking::router())
        .nest("/admin", admin::router(state.clone()))
        .merge(streams::router())
        .merge(orders::router())
        .merge(customer_admin::router(state.clone()))
        .merge(crate::student::router(state.clone()))
        .merge(crate::merchant::router(state.clone()))
        .merge(crate::handoff::router(state.clone()))
        .merge(customer_login::router())
}
