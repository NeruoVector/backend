use axum::Router;

use crate::state::AppState;

pub mod deliveries;
pub mod detections;
pub mod flights;
pub mod inventory;
pub mod risk;
pub mod route_plans;
pub mod streams;

pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/detections", detections::router())
        .nest("/routes", route_plans::router())
        .nest("/deliveries", deliveries::router())
        .nest("/flights", flights::router())
        .nest("/risk", risk::router())
        .nest("/inventory", inventory::router())
        .merge(streams::router())
}
