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
mod geo;
mod ingest;
mod media;
mod models;
mod mqtt;
mod routes;
mod state;

use events::KafkaPublisher;
use media::MediaClient;
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

    if let Err(err) = tokio::fs::create_dir_all(&config.media_recordings_dir).await {
        tracing::warn!(
            path = %config.media_recordings_dir.display(),
            error = %err,
            "failed to create recordings directory"
        );
    }

    let state = AppState {
        pool,
        redis,
        kafka: KafkaPublisher::new(
            config.kafka_brokers.clone(),
            config.kafka_topic_events.clone(),
        ),
        media: MediaClient::new(
            config.media_api_base_url.clone(),
            config.media_whep_base_url.clone(),
            config.media_srt_public_host.clone(),
            config.media_srt_port,
            config.media_recordings_dir.clone(),
        ),
        max_altitude_m: config.max_altitude_m,
        route_max_leg_m: config.route_max_leg_m,
        route_max_distance_m: config.route_max_distance_m,
        route_battery_wh: config.route_battery_wh,
        route_power_w: config.route_power_w,
        risk_lookback_days: config.risk_lookback_days,
        risk_corridor_m: config.risk_corridor_m,
        telemetry_online_ttl_seconds: config.telemetry_online_ttl_seconds,
    };

    mqtt::spawn(state.clone(), &config);

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
        docs: "/health, /health/ready, /api/v1/detections, /api/v1/routes, /api/v1/deliveries, /api/v1/flights, /api/v1/risk, /api/v1/orders, /api/v1/streams, /api/v1/recordings",
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
