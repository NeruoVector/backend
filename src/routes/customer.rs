use super::users::current_user;
use crate::{
    auth::{self, CurrentAdmin},
    error::AppError,
    state::AppState,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    routing::{get, post, put},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub fn router(state: AppState) -> Router<AppState> {
    let admin = Router::new()
        .route("/admin/notifications", post(send_notification))
        .route("/admin/schools/{code}", put(save_school))
        .route_layer(middleware::from_fn_with_state(state, auth::require_admin));
    Router::new()
        .merge(admin)
        .route("/schools", get(list_schools))
        .route("/schools/{code}", get(get_school))
        .route("/reviews", get(list_review_feed).post(create_review))
        .route("/reviews/{id}", put(update_review).delete(delete_review))
        .route("/notifications", get(list_notifications))
        .route("/notifications/{id}/read", post(read_notification))
        .route("/phone-verifications", post(request_phone))
        .route("/phone-verifications/confirm", post(confirm_phone))
        .route("/payment-methods", post(save_payment_method))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Deserialize, IntoParams)]
pub struct Page {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
impl Page {
    fn bounds(&self) -> Result<(i64, i64), AppError> {
        let limit = self.limit.unwrap_or(50);
        let offset = self.offset.unwrap_or(0);
        if !(1..=200).contains(&limit) || offset < 0 {
            return Err(AppError::BadRequest(
                "limit은 1~200, offset은 0 이상이어야 합니다.".into(),
            ));
        }
        Ok((limit, offset))
    }
}

fn text_length(value: &str, max: usize) -> Result<&str, AppError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(AppError::BadRequest(format!(
            "문자열은 1~{max}자여야 합니다."
        )));
    }
    Ok(value)
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct School {
    pub code: String,
    pub name: String,
    pub region: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SchoolInput {
    pub name: String,
    pub region: String,
}
#[derive(Deserialize, IntoParams)]
pub struct SchoolQuery {
    pub q: Option<String>,
    pub region: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[utoipa::path(get, path="/api/v1/schools", params(SchoolQuery), responses((status=200, description="등록된 학교 코드/이름 검색", body=[School])), tag="schools")]
pub async fn list_schools(
    State(state): State<AppState>,
    Query(query): Query<SchoolQuery>,
) -> Result<Json<Vec<School>>, AppError> {
    let (limit, offset) = Page {
        limit: query.limit,
        offset: query.offset,
    }
    .bounds()?;
    let search = query.q.as_deref().unwrap_or("").trim();
    if search.chars().count() > 200 {
        return Err(AppError::BadRequest("검색어가 너무 깁니다.".into()));
    }
    Ok(Json(sqlx::query_as::<_, School>("SELECT code,name,region FROM neurovector.schools WHERE (strpos(lower(name),lower($1))>0 OR strpos(lower(code),lower($1))>0) AND ($2::text IS NULL OR region=$2) ORDER BY name,code LIMIT $3 OFFSET $4")
        .bind(search).bind(query.region.as_deref()).bind(limit).bind(offset).fetch_all(&state.pool).await?))
}

#[utoipa::path(get, path="/api/v1/schools/{code}", params(("code"=String, Path)), responses((status=200, description="학교 코드 조회", body=School),(status=404, description="등록되지 않은 학교")), tag="schools")]
pub async fn get_school(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<School>, AppError> {
    sqlx::query_as::<_, School>("SELECT code,name,region FROM neurovector.schools WHERE code=$1")
        .bind(code)
        .fetch_optional(&state.pool)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

#[utoipa::path(put, path="/api/v1/admin/schools/{code}", params(("code"=String, Path)), request_body=SchoolInput, security(("admin_session"=[])), responses((status=200, description="학교 등록/수정", body=School),(status=401, description="관리자 로그인 필요")), tag="schools")]
pub async fn save_school(
    State(state): State<AppState>,
    Extension(_admin): Extension<CurrentAdmin>,
    Path(code): Path<String>,
    Json(body): Json<SchoolInput>,
) -> Result<Json<School>, AppError> {
    Ok(Json(sqlx::query_as::<_, School>("INSERT INTO neurovector.schools(code,name,region) VALUES($1,$2,$3) ON CONFLICT(code) DO UPDATE SET name=excluded.name,region=excluded.region RETURNING code,name,region")
        .bind(text_length(&code,40)?).bind(text_length(&body.name,200)?).bind(text_length(&body.region,100)?).fetch_one(&state.pool).await?))
}

#[derive(Serialize, FromRow, ToSchema)]
#[schema(as = ItemReview)]
pub struct Review {
    pub id: Uuid,
    pub user_id: Uuid,
    pub item_id: Uuid,
    pub rating: i16,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    pub item_id: Uuid,
    pub rating: i16,
    pub content: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewUpdate {
    pub rating: i16,
    pub content: String,
}
#[derive(Deserialize, IntoParams)]
pub struct ReviewQuery {
    pub item_id: Uuid,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
fn review_content(rating: i16, content: &str) -> Result<&str, AppError> {
    if !(1..=5).contains(&rating) {
        return Err(AppError::BadRequest("평점은 1~5여야 합니다.".into()));
    }
    text_length(content, 2000)
}

#[derive(Deserialize, IntoParams)]
pub struct ReviewFeedQuery {
    pub item_id: Option<Uuid>,
    pub phone: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum ReviewFeed {
    Items(Vec<Review>),
    Orders(super::reviews::ReviewList),
}

#[utoipa::path(get, path="/api/v1/reviews", params(ReviewFeedQuery), responses((status=200, description="item_id가 있으면 상품 리뷰 배열, 없으면 주문 리뷰 목록과 통계", body=ReviewFeed),(status=400, description="잘못된 조회 조건")), tag="reviews")]
pub async fn list_review_feed(
    State(state): State<AppState>,
    Query(query): Query<ReviewFeedQuery>,
) -> Result<Json<ReviewFeed>, AppError> {
    match query.item_id {
        Some(item_id) => {
            let Json(items) = list_reviews(
                State(state),
                Query(ReviewQuery {
                    item_id,
                    limit: query.limit,
                    offset: query.offset,
                }),
            )
            .await?;
            Ok(Json(ReviewFeed::Items(items)))
        }
        None => {
            let Json(reviews) = super::reviews::list_reviews(
                State(state),
                Query(super::reviews::ListParams {
                    phone: query.phone,
                    limit: query.limit,
                }),
            )
            .await?;
            Ok(Json(ReviewFeed::Orders(reviews)))
        }
    }
}

#[utoipa::path(get, path="/api/v1/reviews", params(ReviewQuery), responses((status=200, description="상품 리뷰 목록", body=[Review])), tag="reviews")]
pub async fn list_reviews(
    State(state): State<AppState>,
    Query(query): Query<ReviewQuery>,
) -> Result<Json<Vec<Review>>, AppError> {
    let (limit, offset) = Page {
        limit: query.limit,
        offset: query.offset,
    }
    .bounds()?;
    Ok(Json(sqlx::query_as::<_, Review>("SELECT * FROM neurovector.reviews WHERE item_id=$1 ORDER BY created_at DESC,id DESC LIMIT $2 OFFSET $3")
        .bind(query.item_id).bind(limit).bind(offset).fetch_all(&state.pool).await?))
}

#[utoipa::path(post, path="/api/v1/reviews", request_body=ReviewInput, security(("user_session"=[])), responses((status=201, description="리뷰 작성", body=Review),(status=400, description="입력 오류"),(status=401, description="사용자 로그인 필요"),(status=404, description="상품 없음"),(status=409, description="이미 리뷰 작성")), tag="reviews")]
pub async fn create_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReviewInput>,
) -> Result<(StatusCode, Json<Review>), AppError> {
    let user = current_user(&state, &headers).await?;
    let content = review_content(body.rating, &body.content)?;
    let result=sqlx::query_as::<_, Review>("INSERT INTO neurovector.reviews(user_id,item_id,rating,content) VALUES($1,$2,$3,$4) ON CONFLICT(user_id,item_id) DO NOTHING RETURNING *")
        .bind(user.user_id).bind(body.item_id).bind(body.rating).bind(content).fetch_optional(&state.pool).await;
    match result {
        Ok(Some(review)) => Ok((StatusCode::CREATED, Json(review))),
        Ok(None) => Err(AppError::Conflict(
            "이 상품에 이미 리뷰를 작성했습니다.".into(),
        )),
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Err(AppError::NotFound),
        Err(e) => Err(e.into()),
    }
}

#[utoipa::path(put, path="/api/v1/reviews/{id}", params(("id"=Uuid, Path)), request_body=ReviewUpdate, security(("user_session"=[])), responses((status=200, description="본인 리뷰 수정", body=Review),(status=404, description="본인 리뷰 없음"),(status=401, description="로그인 필요")), tag="reviews")]
pub async fn update_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewUpdate>,
) -> Result<Json<Review>, AppError> {
    let user = current_user(&state, &headers).await?;
    sqlx::query_as::<_, Review>("UPDATE neurovector.reviews SET rating=$1,content=$2,updated_at=now() WHERE id=$3 AND user_id=$4 RETURNING *")
        .bind(body.rating).bind(review_content(body.rating,&body.content)?).bind(id).bind(user.user_id)
        .fetch_optional(&state.pool).await?.map(Json).ok_or(AppError::NotFound)
}

#[utoipa::path(delete, path="/api/v1/reviews/{id}", params(("id"=Uuid, Path)), security(("user_session"=[])), responses((status=204, description="본인 리뷰 삭제"),(status=404, description="본인 리뷰 없음"),(status=401, description="로그인 필요")), tag="reviews")]
pub async fn delete_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let user = current_user(&state, &headers).await?;
    let result = sqlx::query("DELETE FROM neurovector.reviews WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(user.user_id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationInput {
    pub user_id: Uuid,
    pub title: String,
    pub body: String,
}

#[utoipa::path(post, path="/api/v1/admin/notifications", request_body=NotificationInput, security(("admin_session"=[])), responses((status=201, description="사용자 알림함에 발송", body=Notification),(status=401, description="관리자 로그인 필요"),(status=404, description="사용자 없음")), tag="notifications")]
pub async fn send_notification(
    State(state): State<AppState>,
    Extension(_admin): Extension<CurrentAdmin>,
    Json(body): Json<NotificationInput>,
) -> Result<(StatusCode, Json<Notification>), AppError> {
    let result=sqlx::query_as::<_, Notification>("INSERT INTO neurovector.notifications(user_id,title,body) SELECT id,$2,$3 FROM neurovector.users WHERE id=$1 RETURNING *")
        .bind(body.user_id).bind(text_length(&body.title,100)?).bind(text_length(&body.body,4000)?).fetch_optional(&state.pool).await?.ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(result)))
}

#[utoipa::path(get, path="/api/v1/notifications", params(Page), security(("user_session"=[])), responses((status=200, description="본인 알림 목록", body=[Notification]),(status=401, description="사용자 로그인 필요")), tag="notifications")]
pub async fn list_notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Notification>>, AppError> {
    let user = current_user(&state, &headers).await?;
    let (limit, offset) = page.bounds()?;
    Ok(Json(sqlx::query_as::<_, Notification>("SELECT * FROM neurovector.notifications WHERE user_id=$1 ORDER BY created_at DESC,id DESC LIMIT $2 OFFSET $3")
        .bind(user.user_id).bind(limit).bind(offset).fetch_all(&state.pool).await?))
}

#[utoipa::path(post, path="/api/v1/notifications/{id}/read", params(("id"=Uuid, Path)), security(("user_session"=[])), responses((status=200, description="본인 알림 읽음 처리", body=Notification),(status=404, description="본인 알림 없음"),(status=401, description="사용자 로그인 필요")), tag="notifications")]
pub async fn read_notification(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Notification>, AppError> {
    let user = current_user(&state, &headers).await?;
    sqlx::query_as::<_, Notification>("UPDATE neurovector.notifications SET read_at=coalesce(read_at,now()) WHERE id=$1 AND user_id=$2 RETURNING *")
        .bind(id).bind(user.user_id).fetch_optional(&state.pool).await?.map(Json).ok_or(AppError::NotFound)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PhoneRequest {
    pub phone: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PhoneConfirm {
    pub verification_id: Uuid,
    pub code: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentMethodInput {
    /// PG에서 발급한 토큰. 카드 번호/CVC를 받지 않습니다.
    pub provider: String,
    pub billing_token: String,
}
#[derive(Serialize, ToSchema)]
pub struct Placeholder {
    pub error: &'static str,
    pub feature: &'static str,
    pub message: &'static str,
}
fn placeholder(feature: &'static str) -> (StatusCode, Json<Placeholder>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(Placeholder {
            error: "not_implemented",
            feature,
            message: "API 형식만 정의되어 있으며 실제 처리 및 저장은 수행하지 않습니다.",
        }),
    )
}
#[utoipa::path(post, path="/api/v1/phone-verifications", request_body=PhoneRequest, responses((status=501, description="휴대폰 인증 발송 미구현", body=Placeholder)), tag="placeholders")]
pub async fn request_phone(Json(body): Json<PhoneRequest>) -> (StatusCode, Json<Placeholder>) {
    drop(body.phone);
    placeholder("phone_verification")
}
#[utoipa::path(post, path="/api/v1/phone-verifications/confirm", request_body=PhoneConfirm, responses((status=501, description="인증 확인 미구현", body=Placeholder)), tag="placeholders")]
pub async fn confirm_phone(Json(body): Json<PhoneConfirm>) -> (StatusCode, Json<Placeholder>) {
    drop((body.verification_id, body.code));
    placeholder("phone_verification_confirm")
}
#[utoipa::path(post, path="/api/v1/payment-methods", request_body=PaymentMethodInput, security(("user_session"=[])), responses((status=501, description="결제수단 저장 미구현", body=Placeholder),(status=401, description="사용자 로그인 필요")), tag="placeholders")]
pub async fn save_payment_method(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PaymentMethodInput>,
) -> Result<(StatusCode, Json<Placeholder>), AppError> {
    current_user(&state, &headers).await?;
    drop((body.provider, body.billing_token));
    Ok(placeholder("payment_method_storage"))
}
