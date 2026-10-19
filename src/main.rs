use std::net::SocketAddr;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod db;
mod error;
mod events;
mod routes;
mod state;

use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::Config::from_env()?;
    tracing::info!(?config.public_base_url, "starting neurovector-backend");

    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .connect(&config.database_url)
        .await?;

    let redis = redis::Client::open(config.redis_url.clone())?;

    let state = AppState {
        pool,
        redis,
        kafka_brokers: config.kafka_brokers.clone(),
        kafka_topic_events: config.kafka_topic_events.clone(),
    };

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/health/ready", get(health_ready))
        .nest("/api/v1", routes::router())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = config.listen_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");

    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Serialize)]
struct RootResponse {
    name: &'static str,
    version: &'static str,
    docs: &'static str,
}

async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        name: "neurovector-backend",
        version: env!("CARGO_PKG_VERSION"),
        docs: "/health, /health/ready, /api/v1/detections",
    })
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[derive(Serialize)]
struct ReadinessResponse {
    status: &'static str,
    postgres: &'static str,
    redis: &'static str,
}

async fn health_ready(State(state): State<AppState>) -> (StatusCode, Json<ReadinessResponse>) {
    let db_ok = db::ping(&state.pool).await.is_ok();

    let redis_ok = {
        use redis::AsyncCommands;
        let mut conn = match state.redis.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ReadinessResponse {
                        status: "degraded",
                        postgres: if db_ok { "ok" } else { "down" },
                        redis: "down",
                    }),
                );
            }
        };
        conn.ping::<String>().await.is_ok()
    };

    let ok = db_ok && redis_ok;
    let body = ReadinessResponse {
        status: if ok { "ok" } else { "degraded" },
        postgres: if db_ok { "ok" } else { "down" },
        redis: if redis_ok { "ok" } else { "down" },
    };

    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    )
}
