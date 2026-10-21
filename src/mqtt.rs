use std::time::Duration;

use chrono::{DateTime, Utc};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use tokio::time::sleep;

use crate::{
    config::Config,
    ingest,
    models::{CreateDetection, TelemetryMessage},
    state::AppState,
};

#[derive(Clone)]
struct MqttConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    client_id: String,
    topic_prefix: String,
    qos: QoS,
}

impl From<&Config> for MqttConfig {
    fn from(config: &Config) -> Self {
        Self {
            host: config.mqtt_host.clone(),
            port: config.mqtt_port,
            username: config.mqtt_username.clone(),
            password: config.mqtt_password.clone(),
            client_id: config.mqtt_client_id.clone(),
            topic_prefix: config.mqtt_topic_prefix.clone(),
            qos: match config.mqtt_qos {
                0 => QoS::AtMostOnce,
                2 => QoS::ExactlyOnce,
                _ => QoS::AtLeastOnce,
            },
        }
    }
}

/// 백그라운드 MQTT 수신 루프를 시작한다.
pub fn spawn(state: AppState, config: &Config) {
    let mqtt_config = MqttConfig::from(config);
    let detections_topic = format!("{}/drones/+/detections", mqtt_config.topic_prefix);
    let telemetry_topic = format!("{}/drones/+/telemetry", mqtt_config.topic_prefix);
    let status_topic = format!("{}/drones/+/status", mqtt_config.topic_prefix);

    tokio::spawn(async move {
        run(
            state,
            mqtt_config,
            detections_topic,
            telemetry_topic,
            status_topic,
        )
        .await;
    });
}

async fn run(
    state: AppState,
    config: MqttConfig,
    detections_topic: String,
    telemetry_topic: String,
    status_topic: String,
) -> ! {
    let mut backoff_seconds = 1u64;

    loop {
        match connect_and_run(
            &state,
            &config,
            &detections_topic,
            &telemetry_topic,
            &status_topic,
        )
        .await
        {
            Ok(()) => {
                tracing::warn!("MQTT event loop ended; reconnecting");
                backoff_seconds = 1;
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    retry_in_seconds = backoff_seconds,
                    "MQTT connection failed"
                );
            }
        }

        sleep(Duration::from_secs(backoff_seconds)).await;
        backoff_seconds = (backoff_seconds * 2).min(30);
    }
}

async fn connect_and_run(
    state: &AppState,
    config: &MqttConfig,
    detections_topic: &str,
    telemetry_topic: &str,
    status_topic: &str,
) -> anyhow::Result<()> {
    let mut options = MqttOptions::new(config.client_id.clone(), config.host.clone(), config.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_credentials(config.username.clone(), config.password.clone());
    options.set_clean_session(false);

    let (client, mut eventloop) = AsyncClient::new(options, 64);
    client.subscribe(detections_topic, config.qos).await?;
    client.subscribe(telemetry_topic, config.qos).await?;
    client.subscribe(status_topic, config.qos).await?;

    tracing::info!(
        host = %config.host,
        port = config.port,
        detections_topic,
        telemetry_topic,
        "MQTT subscribed"
    );

    loop {
        match eventloop.poll().await? {
            Event::Incoming(Packet::ConnAck(_)) => {
                tracing::info!("MQTT connected");
            }
            Event::Incoming(Packet::Publish(publish)) => {
                handle_publish(state, config, &publish.topic, publish.payload.as_ref()).await;
            }
            _ => {}
        }
    }
}

async fn handle_publish(state: &AppState, config: &MqttConfig, topic: &str, payload: &[u8]) {
    let Some((drone_id, kind)) = parse_topic(topic, &config.topic_prefix) else {
        tracing::debug!(topic, "ignoring MQTT message on unknown topic");
        return;
    };

    match kind {
        "detections" => {
            let payload = match serde_json::from_slice::<DetectionPayload>(payload) {
                Ok(payload) => payload,
                Err(err) => {
                    tracing::warn!(topic, error = %err, "invalid detection payload");
                    return;
                }
            };

            if let Some(payload_drone_id) = &payload.drone_id
                && payload_drone_id != drone_id
            {
                tracing::warn!(
                    topic,
                    topic_drone_id = drone_id,
                    payload_drone_id,
                    "drone_id mismatch; dropping detection"
                );
                return;
            }

            let body = CreateDetection {
                drone_id: drone_id.to_string(),
                label: payload.label,
                confidence: payload.confidence,
                latitude: payload.latitude,
                longitude: payload.longitude,
                distance_m: payload.distance_m,
                priority_level: payload.priority_level,
                detected_at: payload.detected_at,
            };

            match ingest::ingest_detection(state, body).await {
                Ok(detection) => {
                    tracing::debug!(detection_id = %detection.id, drone_id, "MQTT detection stored");
                }
                Err(err) => {
                    tracing::warn!(topic, error = %err, "failed to store MQTT detection");
                }
            }
        }
        "telemetry" => {
            let payload = match serde_json::from_slice::<TelemetryMessage>(payload) {
                Ok(payload) => payload,
                Err(err) => {
                    tracing::warn!(topic, error = %err, "invalid telemetry payload");
                    return;
                }
            };

            match ingest::ingest_telemetry(state, drone_id.to_string(), payload).await {
                Ok(telemetry) => {
                    tracing::debug!(
                        drone_id,
                        altitude_m = telemetry.altitude_m,
                        battery_pct = telemetry.battery_pct,
                        "MQTT telemetry stored"
                    );
                }
                Err(err) => {
                    tracing::warn!(topic, error = %err, "failed to store MQTT telemetry");
                }
            }
        }
        "status" => {
            handle_status(state, drone_id, payload).await;
        }
        _ => {}
    }
}

async fn handle_status(state: &AppState, drone_id: &str, payload: &[u8]) {
    #[derive(Deserialize)]
    struct StatusPayload {
        #[serde(default)]
        online: Option<bool>,
        #[serde(default)]
        status: Option<String>,
    }

    let payload = serde_json::from_slice::<StatusPayload>(payload).unwrap_or(StatusPayload {
        online: Some(false),
        status: None,
    });
    let online = payload.online.unwrap_or(true);
    let key = format!("neurovector:drone:{drone_id}:online");
    let ttl = if online { 120 } else { 3600 };
    let value = serde_json::json!({
        "online": online,
        "status": payload.status,
        "updated_at": Utc::now(),
    });

    match state.redis.get_multiplexed_async_connection().await {
        Ok(mut conn) => {
            use redis::AsyncCommands;
            let _: Result<(), _> = conn.set_ex(key, value.to_string(), ttl).await;
        }
        Err(err) => {
            tracing::warn!(error = %err, drone_id, "failed to update drone online status");
        }
    }
}

/// `neurovector/drones/{drone_id}/{kind}` 형태의 토픽을 분해한다.
fn parse_topic<'a>(topic: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = topic.strip_prefix(prefix)?.strip_prefix("/drones/")?;
    let (drone_id, kind) = rest.split_once('/')?;
    if drone_id.is_empty() || kind.is_empty() || kind.contains('/') {
        return None;
    }
    Some((drone_id, kind))
}

#[derive(Debug, Deserialize)]
struct DetectionPayload {
    #[serde(default)]
    drone_id: Option<String>,
    label: String,
    confidence: f32,
    latitude: Option<f64>,
    longitude: Option<f64>,
    distance_m: Option<f64>,
    #[serde(default)]
    priority_level: Option<i16>,
    detected_at: Option<DateTime<Utc>>,
}
