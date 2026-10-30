use crate::{
    error::AppError,
    events::DetectionEvent,
    models::{CreateDetection, Detection},
    state::AppState,
};
use std::time::Duration;

fn default_priority_level(label: &str) -> i16 {
    let normalized = label.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "face" | "body" | "torso" | "arm" | "hand" | "leg" | "foot" | "left_arm" | "right_arm"
        | "left_hand" | "right_hand" | "left_leg" | "right_leg" | "left_foot" | "right_foot"
        | "person" | "people" | "human" | "pedestrian" | "animal" | "cat" | "dog" | "bird"
        | "horse" | "sheep" | "cow" | "elephant" | "bear" | "zebra" | "giraffe" | "얼굴"
        | "신체" | "몸통" | "팔" | "손" | "다리" | "발" | "사람" | "보행자" | "동물" | "고양이"
        | "개" | "강아지" | "새" | "말" | "양" | "소" | "코끼리" | "곰" | "얼룩말" | "기린" => {
            1
        }
        "leaf" | "leaves" | "fallen leaf" | "tree leaf" | "leaf litter" | "grass" | "flower"
        | "flowers" | "petal" | "petals" | "나뭇잎" | "낙엽" | "잎" | "풀" | "꽃" | "꽃잎" => {
            3
        }
        _ => 2,
    }
}

pub async fn ingest_detection(
    state: &AppState,
    body: CreateDetection,
) -> Result<Detection, AppError> {
    if body.drone_id.trim().is_empty() {
        return Err(AppError::BadRequest("drone_id is required".into()));
    }
    if body.label.trim().is_empty() {
        return Err(AppError::BadRequest("label is required".into()));
    }
    if !(0.0..=1.0).contains(&body.confidence) {
        return Err(AppError::BadRequest(
            "confidence must be between 0 and 1".into(),
        ));
    }
    if let Some(lat) = body.latitude
        && !(-90.0..=90.0).contains(&lat)
    {
        return Err(AppError::BadRequest(
            "latitude must be between -90 and 90".into(),
        ));
    }
    if let Some(lon) = body.longitude
        && !(-180.0..=180.0).contains(&lon)
    {
        return Err(AppError::BadRequest(
            "longitude must be between -180 and 180".into(),
        ));
    }
    if let Some(distance) = body.distance_m
        && (!distance.is_finite() || distance < 0.0)
    {
        return Err(AppError::BadRequest(
            "distance_m must be a non-negative finite number".into(),
        ));
    }
    let priority_level = body
        .priority_level
        .unwrap_or_else(|| default_priority_level(&body.label));
    if !(1..=3).contains(&priority_level) {
        return Err(AppError::BadRequest(
            "priority_level must be between 1 and 3".into(),
        ));
    }

    let detection = crate::db::fetch_one::<Detection>(
        &state.pool.get().await?,
        r#"
        INSERT INTO neurovector.detections
            (drone_id, label, confidence, latitude, longitude, distance_m,
             priority_level, detected_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, now()))
        RETURNING id, drone_id, label, confidence, latitude, longitude, distance_m,
                  priority_level, detected_at, created_at
        "#,
        &[
            &(&body.drone_id),
            &(&body.label),
            &(body.confidence),
            &(body.latitude),
            &(body.longitude),
            &(body.distance_m),
            &(priority_level),
            &(body.detected_at),
        ],
    )
    .await?;

    let event = DetectionEvent {
        id: detection.id,
        drone_id: &detection.drone_id,
        label: &detection.label,
        confidence: detection.confidence,
        distance_m: detection.distance_m,
        priority_level: detection.priority_level,
        detected_at: detection.detected_at,
    };

    // Kafka 장애 시 MQTT 수신 루프가 무기한 블록되지 않도록 타임아웃을 둔다.
    match tokio::time::timeout(
        Duration::from_secs(2),
        state.kafka.publish_detection(&event),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(error = %err, detection_id = %detection.id, "failed to publish detection event");
        }
        Err(_) => {
            tracing::warn!(
                detection_id = %detection.id,
                "timed out publishing detection event"
            );
        }
    }

    Ok(detection)
}
