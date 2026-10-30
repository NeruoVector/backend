use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::Response,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::{auth, error::AppError, state::AppState};

#[derive(Debug, Serialize)]
struct VerificationRow {
    customer_id: Uuid,
    customer_name: String,
    school: String,
    student_number: String,
    status: String,
    reason: Option<String>,
    submitted_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
}
crate::db::row!(VerificationRow {
    customer_id,
    customer_name,
    school,
    student_number,
    status,
    reason,
    submitted_at,
    reviewed_at
});

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReviewDecision {
    Approved,
    Rejected,
}

impl ReviewDecision {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReviewRequest {
    status: ReviewDecision,
    #[serde(default)]
    reason: Option<String>,
}

pub(crate) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/admin/student-verifications", get(list_verifications))
        .route("/admin/student-verifications/{id}/evidence", get(evidence))
        .route("/admin/student-verifications/{id}/review", post(review))
        .route_layer(middleware::from_fn_with_state(state, auth::require_admin))
        .route_layer(middleware::from_fn(
            crate::customer_auth::require_same_origin,
        ))
}

async fn list_verifications(
    State(state): State<AppState>,
) -> Result<Json<Vec<VerificationRow>>, AppError> {
    let rows = crate::db::fetch_all::<VerificationRow>(
        &state.pool.get().await?,
        r#"
        SELECT v.customer_id, c.name AS customer_name, v.school, v.student_number,
               v.status, v.reason, v.submitted_at, v.reviewed_at
        FROM neurovector.student_verifications v
        JOIN neurovector.customers c ON c.id = v.customer_id
        ORDER BY CASE WHEN v.status = 'pending' THEN 0 ELSE 1 END, v.submitted_at DESC
        "#,
        &[],
    )
    .await?;
    Ok(Json(rows))
}

async fn evidence(
    State(state): State<AppState>,
    Path(customer_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let (bytes, mime) = crate::db::fetch_optional::<(Vec<u8>, String)>(&state.pool.get().await?, "SELECT evidence, content_type FROM neurovector.student_verifications WHERE customer_id = $1", &[&(customer_id)])
    .await?
    .ok_or(AppError::NotFound)?;
    let content_type = match mime.as_str() {
        "image/png" => HeaderValue::from_static("image/png"),
        "image/jpeg" => HeaderValue::from_static("image/jpeg"),
        "image/webp" => HeaderValue::from_static("image/webp"),
        _ => return Err(AppError::BadRequest("unsupported evidence type".into())),
    };
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

async fn review(
    State(state): State<AppState>,
    Path(customer_id): Path<Uuid>,
    Json(body): Json<ReviewRequest>,
) -> Result<Json<crate::student::VerificationStatus>, AppError> {
    if let Some(reason) = &body.reason
        && reason.chars().count() > 500
    {
        return Err(AppError::BadRequest(
            "reason must be at most 500 characters".into(),
        ));
    }
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let verification = crate::db::fetch_optional::<crate::student::VerificationStatus>(
        &state.pool.get().await?,
        r#"
        UPDATE neurovector.student_verifications
        SET status = $2, reason = $3, reviewed_at = now()
        WHERE customer_id = $1
        RETURNING school, student_number, status, reason, submitted_at, reviewed_at
        "#,
        &[&(customer_id), &(body.status.as_str()), &(reason)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(verification))
}
