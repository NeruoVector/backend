use axum::Router;

use crate::state::AppState;

pub mod admin;
pub mod customer;
pub mod deliveries;
pub mod detections;
pub mod flights;
pub mod inventory;
pub mod orders;
pub mod product_photos;
pub mod receipts;
pub mod reviews;
pub mod risk;
pub mod route_plans;
pub mod simulation;
pub mod streams;
pub mod users;

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
        .merge(receipts::router())
        .merge(product_photos::router(state.clone()))
        .merge(simulation::router())
        .merge(streams::router())
        .merge(orders::router())
        .merge(crate::customer_auth::router())
        .merge(crate::oauth::router())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::customer_auth::protect,
        ))
        .merge(customer::router(state))
        .merge(users::router())
}

#[cfg(test)]
mod customer_tests;

#[cfg(test)]
mod merge_tests;
