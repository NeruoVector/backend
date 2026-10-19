use axum::Router;

use crate::state::AppState;

pub mod detections;

pub fn router() -> Router<AppState> {
    Router::new().nest("/detections", detections::router())
}
