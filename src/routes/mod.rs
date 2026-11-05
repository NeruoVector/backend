use axum::Router;

use crate::state::AppState;

pub mod admin;
pub mod deliveries;
pub mod detections;
pub mod flights;
pub mod inventory;
pub mod orders;
pub mod reviews;
pub mod risk;
pub mod route_plans;
pub mod simulation;
pub mod streams;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .nest("/detections", detections::router())
        .nest("/routes", route_plans::router())
        .nest("/deliveries", deliveries::router())
        .nest("/flights", flights::router())
        .nest("/risk", risk::router())
        .nest("/inventory", inventory::router(state.clone()))
        .nest("/admin", admin::router(state.clone()))
        .merge(reviews::router())
        .merge(simulation::router())
        .merge(streams::router())
        .merge(orders::router())
        .merge(crate::customer_auth::router())
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::customer_auth::protect,
        ))
}
