use axum::Router;

use crate::state::AppState;

pub mod admin;
pub mod deliveries;
pub mod detections;
pub mod flights;
pub mod inventory;
pub mod orders;
pub mod risk;
pub mod route_plans;
pub mod streams;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .nest("/detections", detections::router())
        .nest("/routes", route_plans::router())
        .nest("/deliveries", deliveries::router())
        .nest("/flights", flights::router())
        .nest("/risk", risk::router())
        .nest("/inventory", inventory::router(state.clone()))
        .nest("/admin", admin::router(state))
        .merge(streams::router())
        .merge(orders::router())
}
