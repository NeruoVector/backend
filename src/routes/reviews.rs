//! Persist one immutable review per paid order without exposing customer details.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

const TAGS: [&str; 3] = ["맛있어요", "배달이 빨라요", "포장이 깔끔해요"];
const MAX_PHOTO_BYTES: usize = 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/orders/{id}/review", get(get_review).post(create_review))
        .route("/reviews", get(list_reviews))
        .route("/reviews/{id}/photo", get(photo))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateReview {
    #[schema(minimum = 1, maximum = 5, example = 5)]
    rating: i16,
    #[schema(min_length = 1, max_length = 500, example = "맛있게 먹었어요.")]
    content: String,
    /// 맛있어요 / 배달이 빨라요 / 포장이 깔끔해요 중 중복 없이 최대 3개.
    #[serde(default)]
    tags: Vec<String>,
    /// JPG·PNG·WebP의 base64 data URL. 디코딩한 사진 크기 최대 1MB. 사진이 없으면 null.
    photo_data_url: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Review {
    id: Uuid,
    order_id: Uuid,
    rating: i16,
    content: String,
    tags: Vec<String>,
    has_photo: bool,
    created_at: DateTime<Utc>,
}

struct Photo {
    data: Vec<u8>,
    mime: &'static str,
}

impl CreateReview {
    fn validate(&self) -> Result<Option<Photo>, AppError> {
        if !(1..=5).contains(&self.rating)
            || !(1..=500).contains(&self.content.trim().chars().count())
        {
            return Err(AppError::BadRequest(
                "별점은 1~5점, 리뷰 내용은 1~500자로 입력해 주세요.".into(),
            ));
        }
        if self.tags.len() > TAGS.len()
            || self.tags.iter().any(|tag| !TAGS.contains(&tag.as_str()))
            || self
                .tags
                .iter()
                .enumerate()
                .any(|(index, tag)| self.tags[..index].contains(tag))
        {
            return Err(AppError::BadRequest("리뷰 태그를 확인해 주세요.".into()));
        }
        let Some(encoded) = self.photo_data_url.as_deref() else {
            return Ok(None);
        };
        if encoded.len() > MAX_PHOTO_BYTES.div_ceil(3) * 4 + 32 {
            return Err(AppError::BadRequest(
                "사진은 1MB 이하만 등록할 수 있습니다.".into(),
            ));
        }
        let (mime, data) = ["image/jpeg", "image/png", "image/webp"]
            .into_iter()
            .find_map(|mime| {
                encoded
                    .strip_prefix(&format!("data:{mime};base64,"))
                    .map(|data| (mime, data))
            })
            .ok_or_else(|| AppError::BadRequest("사진은 JPG·PNG·WebP 형식만 지원합니다.".into()))?;
        let data = STANDARD
            .decode(data)
            .map_err(|_| AppError::BadRequest("사진 데이터를 읽을 수 없습니다.".into()))?;
        let signature_ok = match mime {
            "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
            "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/webp" => data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !signature_ok || data.len() > MAX_PHOTO_BYTES {
            return Err(AppError::BadRequest(
                "사진 형식 또는 크기를 확인해 주세요.".into(),
            ));
        }
        Ok(Some(Photo { data, mime }))
    }
}

#[utoipa::path(post, path="/api/v1/orders/{id}/review", tag="reviews", params(("id"=Uuid,Path,description="결제 완료 주문 ID")), request_body=CreateReview, responses((status=201,description="리뷰 등록",body=Review),(status=400,description="내용·태그·사진 형식 오류"),(status=404,description="주문 없음"),(status=409,description="미결제·취소 주문 또는 이미 등록된 리뷰")), description="주문당 한 번 등록합니다. 별점 1~5, 내용 1~500자, 허용 태그 최대 3개. 사진은 JPG/PNG/WebP base64 data URL, 디코딩 후 최대 1MB입니다. 중복 요청은 기존 리뷰를 변경하지 않습니다.")]
pub(crate) async fn create_review(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateReview>,
) -> Result<(StatusCode, Json<Review>), AppError> {
    let photo = body.validate()?;
    let mut tx = state.pool.begin().await?;
    let eligible = sqlx::query_scalar::<_, bool>(
        "SELECT status IN ('paid','preparing','ready','assigned','in_transit','delivered') AND EXISTS (SELECT 1 FROM neurovector.payments p WHERE p.order_id=o.id AND p.status='captured') FROM neurovector.orders o WHERE id=$1 FOR SHARE"
    ).bind(id).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    if !eligible {
        return Err(AppError::Conflict(
            "결제가 완료된 주문에 리뷰를 작성할 수 있습니다.".into(),
        ));
    }
    let review = sqlx::query_as::<_, Review>(
        "INSERT INTO neurovector.order_reviews AS r (order_id,rating,content,tags,photo_data,photo_mime) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (order_id) DO NOTHING RETURNING r.id,r.order_id,r.rating,r.content,r.tags,(r.photo_data IS NOT NULL) AS has_photo,r.created_at"
    ).bind(id).bind(body.rating).bind(body.content.trim()).bind(&body.tags)
        .bind(photo.as_ref().map(|photo| photo.data.as_slice()))
        .bind(photo.as_ref().map(|photo| photo.mime))
        .fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::Conflict("이미 리뷰를 등록한 주문입니다.".into()))?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(review)))
}

#[utoipa::path(get,path="/api/v1/orders/{id}/review",tag="reviews",params(("id"=Uuid,Path,description="주문 ID")),responses((status=200,description="저장된 리뷰",body=Review),(status=404,description="등록된 리뷰 없음")))]
pub(crate) async fn get_review(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Review>, AppError> {
    let review = sqlx::query_as::<_, Review>("SELECT r.id,r.order_id,r.rating,r.content,r.tags,(r.photo_data IS NOT NULL) AS has_photo,r.created_at FROM neurovector.order_reviews r WHERE r.order_id=$1")
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(review))
}

#[derive(Deserialize)]
pub struct ListParams {
    phone: Option<String>,
    limit: Option<i64>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ReviewList {
    items: Vec<Review>,
    total: i64,
    average_rating: Option<f64>,
}
#[derive(sqlx::FromRow)]
struct Summary {
    total: i64,
    average_rating: Option<f64>,
}

#[utoipa::path(get,path="/api/v1/reviews",tag="reviews",params(("phone"=Option<String>,Query,description="주문 연락처로 필터, 생략하면 매점 전체 리뷰"),("limit"=Option<i64>,Query,description="1~100, 기본 20")),responses((status=200,description="리뷰 목록과 전체 건수·평균 별점",body=ReviewList)))]
pub(crate) async fn list_reviews(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<ReviewList>, AppError> {
    let phone = params.phone.as_deref().map(str::trim);
    if phone.is_some_and(str::is_empty) {
        return Err(AppError::BadRequest("연락처를 입력해 주세요.".into()));
    }
    let summary = sqlx::query_as::<_, Summary>("SELECT count(*) AS total, avg(r.rating)::float8 AS average_rating FROM neurovector.order_reviews r JOIN neurovector.orders o ON o.id=r.order_id WHERE ($1::text IS NULL OR o.customer_phone=$1)")
        .bind(phone).fetch_one(&state.pool).await?;
    let items = sqlx::query_as::<_, Review>("SELECT r.id,r.order_id,r.rating,r.content,r.tags,(r.photo_data IS NOT NULL) AS has_photo,r.created_at FROM neurovector.order_reviews r JOIN neurovector.orders o ON o.id=r.order_id WHERE ($1::text IS NULL OR o.customer_phone=$1) ORDER BY r.created_at DESC,r.id DESC LIMIT $2")
        .bind(phone).bind(params.limit.unwrap_or(20).clamp(1,100)).fetch_all(&state.pool).await?;
    Ok(Json(ReviewList {
        items,
        total: summary.total,
        average_rating: summary.average_rating,
    }))
}

#[utoipa::path(get,path="/api/v1/reviews/{id}/photo",tag="reviews",params(("id"=Uuid,Path,description="리뷰 ID")),responses((status=200,description="원본 리뷰 사진(JPEG/PNG/WebP)",content((Vec<u8> = "image/jpeg"),(Vec<u8> = "image/png"),(Vec<u8> = "image/webp"))),(status=404,description="리뷰 또는 사진 없음")))]
pub(crate) async fn photo(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let (data, mime): (Vec<u8>, String) = sqlx::query_as("SELECT photo_data,photo_mime FROM neurovector.order_reviews WHERE id=$1 AND photo_data IS NOT NULL")
        .bind(id).fetch_optional(&state.pool).await?.ok_or(AppError::NotFound)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime).map_err(anyhow::Error::from)?,
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400, immutable"),
    );
    Ok((headers, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> CreateReview {
        CreateReview {
            rating: 4,
            content: " 맛있어요 ".into(),
            tags: vec!["맛있어요".into()],
            photo_data_url: None,
        }
    }
    #[test]
    fn rejects_invalid_rating_empty_text_and_unknown_or_duplicate_tags() {
        for rating in [0, 6] {
            let mut input = valid();
            input.rating = rating;
            assert!(input.validate().is_err());
        }
        for content in [" ", "\n\t"] {
            let mut input = valid();
            input.content = content.into();
            assert!(input.validate().is_err());
        }
        let mut input = valid();
        input.content = "가".repeat(501);
        assert!(input.validate().is_err());
        for tags in [
            vec!["없는 태그".into()],
            vec!["맛있어요".into(), "맛있어요".into()],
        ] {
            let mut input = valid();
            input.tags = tags;
            assert!(input.validate().is_err());
        }
    }
    #[test]
    fn accepts_unicode_content_at_limit_and_real_png() {
        let mut input = valid();
        input.content = "가".repeat(500);
        assert!(input.validate().is_ok());
        let data = b"\x89PNG\r\n\x1a\nfixture";
        input.photo_data_url = Some(format!("data:image/png;base64,{}", STANDARD.encode(data)));
        assert_eq!(input.validate().unwrap().unwrap().data, data);
    }
    #[test]
    fn rejects_unsafe_mime_corrupt_base64_mismatched_signature_and_large_photo() {
        for value in [
            "data:image/svg+xml;base64,PHN2Zz4=",
            "data:image/png;base64,!!!",
            "data:image/png;base64,YWJj",
        ] {
            let mut input = valid();
            input.photo_data_url = Some(value.into());
            assert!(input.validate().is_err());
        }
        let mut input = valid();
        input.photo_data_url = Some(format!(
            "data:image/png;base64,{}",
            STANDARD.encode(vec![0; MAX_PHOTO_BYTES + 1])
        ));
        assert!(input.validate().is_err());
    }
}
