use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    geo::{self, Waypoint},
    models::Detection,
    routes::route_plans::fetch_route,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/predict", get(predict))
        .route("/hotspots", get(hotspots))
}

#[derive(Debug, Deserialize)]
pub struct PredictParams {
    #[serde(default)]
    pub drone_id: Option<String>,
    #[serde(default)]
    pub route_id: Option<Uuid>,
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub corridor_m: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct RiskPrediction {
    pub drone_id: Option<String>,
    pub route_id: Option<Uuid>,
    pub horizon_days: i64,
    pub generated_at: DateTime<Utc>,
    pub score: f64,
    pub level: &'static str,
    pub expected_incidents_7d: f64,
    pub sample_count: i64,
    pub trend: RiskTrend,
    pub factors: Vec<RiskFactor>,
    pub recommendation: String,
}

#[derive(Debug, Serialize)]
pub struct RiskTrend {
    pub last_7_days: i64,
    pub previous_7_days: i64,
    pub direction: &'static str,
    pub change_ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct RiskFactor {
    pub label: String,
    pub count: i64,
    pub average_confidence: f64,
    pub weighted_score: f64,
}

#[derive(Debug, Deserialize)]
pub struct HotspotParams {
    #[serde(default)]
    pub drone_id: Option<String>,
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Hotspot {
    pub latitude: f64,
    pub longitude: f64,
    pub incident_count: i64,
    pub average_confidence: f64,
    pub last_seen: DateTime<Utc>,
    pub risk_score: f64,
}

#[derive(Debug, sqlx::FromRow)]
struct HotspotRow {
    latitude: f64,
    longitude: f64,
    incident_count: i64,
    average_confidence: f64,
    last_seen: DateTime<Utc>,
}

async fn predict(
    State(state): State<AppState>,
    Query(params): Query<PredictParams>,
) -> Result<Json<RiskPrediction>, AppError> {
    let days = params
        .days
        .unwrap_or(state.risk_lookback_days)
        .clamp(1, 3650);
    let corridor_m = params.corridor_m.unwrap_or(state.risk_corridor_m).max(1.0);
    let since = Utc::now() - Duration::days(days);
    let now = Utc::now();

    let route = match params.route_id {
        Some(route_id) => Some(fetch_route(&state.pool, route_id).await?),
        None => None,
    };
    let route_points = match &route {
        Some(route) => Some(
            serde_json::from_value::<Vec<Waypoint>>(route.waypoints.clone())
                .map_err(|err| AppError::Other(anyhow::anyhow!("stored route is invalid: {err}")))?
                .iter()
                .map(|waypoint| (waypoint.lat, waypoint.lon))
                .collect::<Vec<_>>(),
        ),
        None => None,
    };

    let drone_id = params
        .drone_id
        .clone()
        .or_else(|| route.as_ref().and_then(|route| route.drone_id.clone()));

    let (min_lat, max_lat, min_lon, max_lon) = match &route_points {
        Some(points) if !points.is_empty() => geo::bounding_box(points, corridor_m),
        _ => (-90.0, 90.0, -180.0, 180.0),
    };

    let mut detections = sqlx::query_as::<_, Detection>(
        r#"
        SELECT id, drone_id, label, confidence, latitude, longitude, distance_m,
               priority_level, detected_at, created_at
        FROM neurovector.detections
        WHERE detected_at >= $1
          AND ($2::text IS NULL OR drone_id = $2)
          AND ($3::float8 IS NULL OR (
              latitude BETWEEN $3 AND $4 AND longitude BETWEEN $5 AND $6
          ))
        ORDER BY detected_at DESC
        LIMIT 5000
        "#,
    )
    .bind(since)
    .bind(drone_id.as_deref())
    .bind(route_points.as_ref().map(|_| min_lat))
    .bind(route_points.as_ref().map(|_| max_lat))
    .bind(route_points.as_ref().map(|_| min_lon))
    .bind(route_points.as_ref().map(|_| max_lon))
    .fetch_all(&state.pool)
    .await?;

    if let Some(points) = &route_points {
        detections.retain(|detection| {
            matches!(
                (detection.latitude, detection.longitude),
                (Some(lat), Some(lon)) if geo::point_to_route_m((lat, lon), points) <= corridor_m
            )
        });
    }

    let mut weighted_total = 0.0;
    let mut factors: HashMap<String, (i64, f64, f64)> = HashMap::new();
    let mut last_7_days = 0i64;
    let mut previous_7_days = 0i64;

    for detection in &detections {
        let age_days = (now - detection.detected_at).num_seconds() as f64 / 86_400.0;
        let recency = 0.5f64.powf(age_days / 30.0);
        let severity = label_severity(&detection.label);
        let weighted = detection.confidence as f64 * severity * recency;
        weighted_total += weighted;

        let entry = factors
            .entry(detection.label.clone())
            .or_insert((0, 0.0, 0.0));
        entry.0 += 1;
        entry.1 += detection.confidence as f64;
        entry.2 += weighted;

        let age_seconds = (now - detection.detected_at).num_seconds();
        if age_seconds <= 7 * 86_400 {
            last_7_days += 1;
        } else if age_seconds <= 14 * 86_400 {
            previous_7_days += 1;
        }
    }

    let sample_count = detections.len() as i64;
    let density = sample_count as f64 / days as f64;
    let score = (weighted_total * 8.0 + density * 15.0).clamp(0.0, 100.0);
    let expected_incidents_7d = weighted_total / days as f64 * 7.0;
    let level = risk_level(score);
    let direction = if last_7_days > previous_7_days {
        "increasing"
    } else if last_7_days < previous_7_days {
        "decreasing"
    } else {
        "stable"
    };
    let change_ratio = if previous_7_days > 0 {
        Some((last_7_days - previous_7_days) as f64 / previous_7_days as f64)
    } else if last_7_days > 0 {
        Some(1.0)
    } else {
        None
    };

    let mut factors = factors
        .into_iter()
        .map(
            |(label, (count, confidence_sum, weighted_score))| RiskFactor {
                label,
                count,
                average_confidence: confidence_sum / count as f64,
                weighted_score,
            },
        )
        .collect::<Vec<_>>();
    factors.sort_by(|a, b| {
        b.weighted_score
            .partial_cmp(&a.weighted_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let recommendation = match level {
        "critical" => "경로를 재검토하고 운항을 일시 중단하는 것을 권장합니다.",
        "high" => "혼잡 시간대를 피하고 감지 밀집 구간을 우회하세요.",
        "medium" => "탐지 이력이 있는 구간을 지날 때 속도를 낮추세요.",
        _ => "현재 데이터 기준 특별한 조치가 필요하지 않습니다.",
    }
    .to_string();

    Ok(Json(RiskPrediction {
        drone_id,
        route_id: params.route_id,
        horizon_days: days,
        generated_at: now,
        score,
        level,
        expected_incidents_7d,
        sample_count,
        trend: RiskTrend {
            last_7_days,
            previous_7_days,
            direction,
            change_ratio,
        },
        factors,
        recommendation,
    }))
}

async fn hotspots(
    State(state): State<AppState>,
    Query(params): Query<HotspotParams>,
) -> Result<Json<Vec<Hotspot>>, AppError> {
    let days = params
        .days
        .unwrap_or(state.risk_lookback_days)
        .clamp(1, 3650);
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let since = Utc::now() - Duration::days(days);

    let rows = sqlx::query_as::<_, HotspotRow>(
        r#"
        SELECT round(latitude::numeric, 3)::float8 AS latitude,
               round(longitude::numeric, 3)::float8 AS longitude,
               COUNT(*) AS incident_count,
               AVG(confidence)::float8 AS average_confidence,
               MAX(detected_at) AS last_seen
        FROM neurovector.detections
        WHERE detected_at >= $1
          AND latitude IS NOT NULL
          AND longitude IS NOT NULL
          AND ($2::text IS NULL OR drone_id = $2)
        GROUP BY 1, 2
        ORDER BY incident_count DESC, last_seen DESC
        LIMIT $3
        "#,
    )
    .bind(since)
    .bind(params.drone_id.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    let hotspots = rows
        .into_iter()
        .map(|row| {
            let risk_score = (row.incident_count as f64 * 12.0 + row.average_confidence * 40.0)
                .clamp(0.0, 100.0);
            Hotspot {
                latitude: row.latitude,
                longitude: row.longitude,
                incident_count: row.incident_count,
                average_confidence: row.average_confidence,
                last_seen: row.last_seen,
                risk_score,
            }
        })
        .collect();

    Ok(Json(hotspots))
}

fn label_severity(label: &str) -> f64 {
    match label.to_ascii_lowercase().as_str() {
        "person" | "pedestrian" | "사람" => 1.0,
        "vehicle" | "car" | "truck" | "차량" => 0.85,
        "bicycle" | "motorcycle" | "자전거" => 0.75,
        "animal" | "dog" | "동물" => 0.55,
        "building" | "structure" => 0.4,
        _ => 0.6,
    }
}

fn risk_level(score: f64) -> &'static str {
    if score >= 75.0 {
        "critical"
    } else if score >= 50.0 {
        "high"
    } else if score >= 25.0 {
        "medium"
    } else {
        "low"
    }
}
