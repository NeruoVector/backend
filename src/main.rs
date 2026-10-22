use std::net::SocketAddr;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode},
    routing::get,
};
use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use tower_http::{
    cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod cache;
mod config;
mod db;
mod docs;
mod error;
mod events;
mod geo;
mod ingest;
mod media;
mod mission;
mod models;
mod mqtt;
mod order_queue;
mod redis_store;
mod routes;
mod state;

use events::KafkaPublisher;
use media::MediaClient;
use state::AppState;
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

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

    auth::bootstrap_admin(&pool, &config.admin_username, &config.admin_password).await?;

    let redis = redis_store::RedisStore::new(
        redis::Client::open(config.redis_url.clone())?,
        std::time::Duration::from_millis(config.redis_timeout_ms),
    );

    if let Err(err) = tokio::fs::create_dir_all(&config.media_recordings_dir).await {
        tracing::warn!(
            path = %config.media_recordings_dir.display(),
            error = %err,
            "failed to create recordings directory"
        );
    }

    let state = AppState {
        pool,
        redis: redis.clone(),
        cache: cache::Cache::new(redis, config.cache_enabled, config.cache_ttl_seconds),
        kafka: KafkaPublisher::new(
            config.kafka_brokers.clone(),
            config.kafka_topic_events.clone(),
        ),
        order_queue: KafkaPublisher::new(
            config.kafka_brokers.clone(),
            config.kafka_topic_orders.clone(),
        ),
        media: MediaClient::new(
            config.media_api_base_url.clone(),
            config.media_whep_base_url.clone(),
            config.media_srt_public_host.clone(),
            config.media_srt_port,
            config.media_recordings_dir.clone(),
        ),
        admin_session_ttl_seconds: config.admin_session_ttl_seconds,
        admin_cookie_secure: config.admin_cookie_secure,
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
    order_queue::spawn_worker(state.clone(), &config);

    let cors_origins = config
        .cors_allowed_origins
        .iter()
        .filter_map(|origin| match origin.parse::<HeaderValue>() {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!(origin, error = %err, "ignoring invalid CORS origin");
                None
            }
        })
        .collect::<Vec<_>>();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(cors_origins))
        .allow_credentials(true)
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request());

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/health/ready", get(health_ready))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", docs::ApiDoc::openapi()))
        .nest("/api/v1", routes::router(state.clone()))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = config.listen_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");

    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Serialize, ToSchema)]
pub(crate) struct RootResponse {
    name: &'static str,
    version: &'static str,
    docs: &'static str,
}

#[utoipa::path(
    get,
    path = "/",
    responses((status = 200, description = "서비스 정보", body = RootResponse)),
    tag = "system"
)]
pub(crate) async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        name: "neurovector-backend",
        version: env!("CARGO_PKG_VERSION"),
        docs: "/swagger-ui, /api-docs/openapi.json, /health, /health/ready, /api/v1/detections, /api/v1/routes, /api/v1/deliveries, /api/v1/flights, /api/v1/risk, /api/v1/orders, /api/v1/admin/auth/login, /api/v1/admin/orders, /api/v1/inventory, /api/v1/streams, /api/v1/recordings",
    })
}

#[derive(Serialize, ToSchema)]
pub(crate) struct HealthResponse {
    status: &'static str,
}

#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, description = "liveness", body = HealthResponse)),
    tag = "system"
)]
pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[derive(Serialize, ToSchema)]
pub(crate) struct ReadinessResponse {
    status: &'static str,
    postgres: &'static str,
    redis: &'static str,
}

#[utoipa::path(
    get,
    path = "/health/ready",
    responses(
        (status = 200, description = "readiness", body = ReadinessResponse),
        (status = 503, description = "Postgres/Redis 중 하나 이상 비정상", body = ReadinessResponse)
    ),
    tag = "system"
)]
pub(crate) async fn health_ready(
    State(state): State<AppState>,
) -> (StatusCode, Json<ReadinessResponse>) {
    let (db_ok, redis_ok) = tokio::join!(
        async {
            matches!(
                tokio::time::timeout(std::time::Duration::from_secs(2), db::ping(&state.pool))
                    .await,
                Ok(Ok(_))
            )
        },
        async {
            use redis::AsyncCommands;
            state
                .redis
                .execute(|mut conn| async move { conn.ping::<String>().await })
                .await
                .is_ok()
        }
    );

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
