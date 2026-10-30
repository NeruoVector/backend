use crate::{
    customer_auth::{self, Customer},
    error::AppError,
    state::AppState,
    student_admin,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    middleware,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::Serialize;

const MAX_EVIDENCE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub(crate) struct VerificationStatus {
    pub school: String,
    pub student_number: String,
    pub status: String,
    pub reason: Option<String>,
    pub submitted_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
}
crate::db::row!(VerificationStatus {
    school,
    student_number,
    status,
    reason,
    submitted_at,
    reviewed_at
});

struct EvidenceUpload {
    school: String,
    student_number: String,
    content_type: &'static str,
    bytes: Vec<u8>,
}

pub fn router(state: AppState) -> Router<AppState> {
    let customer_routes = Router::new()
        .route(
            "/student-verification",
            get(current_verification).post(submit_verification),
        )
        .layer(DefaultBodyLimit::max(MAX_EVIDENCE_BYTES + 16 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            customer_auth::require_customer,
        ))
        .route_layer(middleware::from_fn(customer_auth::require_same_origin));
    Router::new()
        .merge(customer_routes)
        .merge(student_admin::router(state))
}

async fn current_verification(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
) -> Result<Json<Option<VerificationStatus>>, AppError> {
    let verification = crate::db::fetch_optional::<VerificationStatus>(&state.pool.get().await?, "SELECT school, student_number, status, reason, submitted_at, reviewed_at FROM neurovector.student_verifications WHERE customer_id = $1", &[&(customer.id)])
    .await?;
    Ok(Json(verification))
}

async fn submit_verification(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    multipart: Multipart,
) -> Result<Json<VerificationStatus>, AppError> {
    let upload = parse_upload(multipart).await?;
    if upload.school.trim().is_empty() || upload.school.trim().chars().count() > 100 {
        return Err(AppError::BadRequest(
            "school must contain 1 to 100 characters".into(),
        ));
    }
    if upload.student_number.trim().is_empty() || upload.student_number.trim().chars().count() > 40
    {
        return Err(AppError::BadRequest(
            "student_number must contain 1 to 40 characters".into(),
        ));
    }

    let verification = crate::db::fetch_optional::<VerificationStatus>(&state.pool.get().await?, r#"
        INSERT INTO neurovector.student_verifications
            (customer_id, school, student_number, status, evidence, content_type, reason, submitted_at, reviewed_at)
        VALUES ($1, $2, $3, 'pending', $4, $5, NULL, now(), NULL)
        ON CONFLICT (customer_id) DO UPDATE SET
            school = EXCLUDED.school,
            student_number = EXCLUDED.student_number,
            status = 'pending', evidence = EXCLUDED.evidence,
            content_type = EXCLUDED.content_type, reason = NULL,
            submitted_at = now(), reviewed_at = NULL
        WHERE neurovector.student_verifications.status <> 'approved'
        RETURNING school, student_number, status, reason, submitted_at, reviewed_at
        "#, &[&(customer.id), &(upload.school.trim()), &(upload.student_number.trim()), &(upload.bytes), &(upload.content_type)])
    .await?
    .ok_or_else(|| AppError::Conflict("approved verification cannot be replaced".into()))?;
    Ok(Json(verification))
}

async fn parse_upload(mut multipart: Multipart) -> Result<EvidenceUpload, AppError> {
    let mut school = None;
    let mut student_number = None;
    let mut image = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("invalid multipart form".into()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "school" => {
                school = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| AppError::BadRequest("invalid school".into()))?,
                )
            }
            "student_number" => {
                student_number = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| AppError::BadRequest("invalid student_number".into()))?,
                )
            }
            "image" => {
                let mime = field.content_type().unwrap_or_default().to_owned();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|_| AppError::BadRequest("invalid evidence image".into()))?;
                let content_type = checked_image_type(&mime, &bytes).ok_or_else(|| {
                    AppError::BadRequest("image must be a valid PNG, JPEG, or WebP".into())
                })?;
                image = Some((content_type, bytes.to_vec()));
            }
            _ => {}
        }
    }
    let (content_type, bytes) =
        image.ok_or_else(|| AppError::BadRequest("image is required".into()))?;
    Ok(EvidenceUpload {
        school: school.ok_or_else(|| AppError::BadRequest("school is required".into()))?,
        student_number: student_number
            .ok_or_else(|| AppError::BadRequest("student_number is required".into()))?,
        content_type,
        bytes,
    })
}

fn checked_image_type(mime: &str, bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() > MAX_EVIDENCE_BYTES {
        return None;
    }
    match mime {
        "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some("image/png"),
        "image/jpeg" if bytes.starts_with(&[0xff, 0xd8, 0xff]) => Some("image/jpeg"),
        "image/webp"
            if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" =>
        {
            Some("image/webp")
        }
        _ => None,
    }
}
