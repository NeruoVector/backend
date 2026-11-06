use crate::{customer_auth, error::AppError, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    routing::{delete, get},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Copy, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DemoKind {
    #[default]
    Card,
    BankAccount,
    Mobile,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CardInput {
    pub kind: DemoKind,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CardMetadata {
    #[serde(default)]
    pub kind: DemoKind,
    pub nickname: String,
    pub last4: String,
}
impl CardMetadata {
    fn generate(kind: DemoKind) -> Result<Self, AppError> {
        use aes_gcm::aead::{OsRng, rand_core::RngCore};
        let nickname = match kind {
            DemoKind::Card => "카드",
            DemoKind::BankAccount => "계좌",
            DemoKind::Mobile => "휴대폰",
        };
        let mut last4 = String::with_capacity(4);
        while last4.len() < 4 {
            let mut bytes = [0; 1];
            OsRng
                .try_fill_bytes(&mut bytes)
                .map_err(|_| unavailable())?;
            if bytes[0] < 250 {
                last4.push(char::from(b'0' + bytes[0] % 10));
            }
        }
        Ok(Self {
            kind,
            nickname: nickname.into(),
            last4,
        })
    }
}
#[derive(Serialize, ToSchema)]
pub struct Card {
    pub kind: DemoKind,
    pub is_demo: bool,
    pub id: Uuid,
    pub nickname: String,
    pub last4: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
pub struct CardList {
    pub items: Vec<Card>,
    pub storage_available: bool,
    pub payment_enabled: bool,
}
#[derive(sqlx::FromRow)]
pub(crate) struct StoredCard {
    pub id: Uuid,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_id: String,
    pub format_version: i16,
    pub created_at: DateTime<Utc>,
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/cards", get(list).post(create))
        .route("/auth/cards/{id}", delete(remove))
        .layer(axum::middleware::map_response(private_response))
}
async fn private_response(mut response: axum::response::Response) -> axum::response::Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("Cookie"));
    response
}
async fn owner(state: &AppState, headers: &HeaderMap) -> Result<Uuid, AppError> {
    customer_auth::current(state, headers)
        .await?
        .map(|u| u.id)
        .ok_or_else(|| AppError::Unauthorized("로그인이 필요합니다.".into()))
}
#[utoipa::path(get,path="/api/v1/auth/cards",tag="customer cards",responses((status=200,body=CardList),(status=401,description="로그인 필요"),(status=503,description="복호화 불가")))]
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CardList>, AppError> {
    let user = owner(&state, &headers).await?;
    let rows: Vec<StoredCard> = sqlx::query_as("SELECT id,ciphertext,nonce,key_id,format_version,created_at FROM neurovector.customer_card_metadata WHERE user_id=$1 ORDER BY created_at DESC,id").bind(user).fetch_all(&state.pool).await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let key = state.card_metadata_key.as_ref().ok_or_else(unavailable)?;
        let input = key.decrypt(user, &row)?;
        items.push(Card {
            kind: input.kind,
            is_demo: true,
            id: row.id,
            nickname: input.nickname,
            last4: input.last4,
            created_at: row.created_at,
        });
    }
    Ok(Json(CardList {
        items,
        storage_available: state.card_metadata_key.is_some(),
        payment_enabled: false,
    }))
}
fn unavailable() -> AppError {
    AppError::ServiceUnavailable("카드 메모 암호화 저장소를 사용할 수 없습니다.".into())
}
#[utoipa::path(post,path="/api/v1/auth/cards",tag="customer cards",request_body=CardInput,responses((status=201,body=Card),(status=422,description="허용되지 않은 종류 또는 필드"),(status=401,description="로그인 필요"),(status=503,description="저장 불가")))]
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CardInput>,
) -> Result<(StatusCode, Json<Card>), AppError> {
    let user = owner(&state, &headers).await?;
    let input = CardMetadata::generate(input.kind)?;
    let key = state.card_metadata_key.as_ref().ok_or_else(unavailable)?;
    let id = Uuid::new_v4();
    let (nonce, ciphertext) = key.encrypt(user, id, &input)?;
    let created_at = sqlx::query_scalar("INSERT INTO neurovector.customer_card_metadata(id,user_id,ciphertext,nonce,key_id,format_version) VALUES($1,$2,$3,$4,$5,1) RETURNING created_at").bind(id).bind(user).bind(ciphertext).bind(nonce).bind(key.id()).fetch_one(&state.pool).await?;
    Ok((
        StatusCode::CREATED,
        Json(Card {
            kind: input.kind,
            is_demo: true,
            id,
            nickname: input.nickname,
            last4: input.last4,
            created_at,
        }),
    ))
}
#[utoipa::path(delete,path="/api/v1/auth/cards/{id}",tag="customer cards",params(("id"=Uuid,Path)),responses((status=204,description="삭제 완료"),(status=401,description="로그인 필요"),(status=404,description="메모 없음")))]
pub(crate) async fn remove(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let user = owner(&state, &headers).await?;
    let result =
        sqlx::query("DELETE FROM neurovector.customer_card_metadata WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(user)
            .execute(&state.pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
#[cfg(test)]
mod tests;
