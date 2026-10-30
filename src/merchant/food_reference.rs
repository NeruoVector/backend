use axum::{
    Extension, Json,
    extract::{Query, State},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::{customer_auth::Customer, error::AppError, state::AppState};

#[derive(Deserialize)]
pub(super) struct Search {
    #[serde(default)]
    query: String,
    category: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct Item {
    food_code: String,
    name: String,
    manufacturer: String,
    major_category_code: String,
    major_category: String,
    representative_name: String,
    subcategory: String,
    food_weight: String,
    weight_grams: Option<i32>,
    nutrition_basis: String,
    report_number: String,
    source_name: String,
    reference_date: Option<NaiveDate>,
}
crate::db::row!(Item {
    food_code,
    name,
    manufacturer,
    major_category_code,
    major_category,
    representative_name,
    subcategory,
    food_weight,
    weight_grams,
    nutrition_basis,
    report_number,
    source_name,
    reference_date
});

#[derive(Serialize)]
struct Dataset {
    name: String,
    date: NaiveDate,
    record_count: i64,
    source_url: String,
    sha256: String,
}
crate::db::row!(Dataset {
    name,
    date,
    record_count,
    source_url,
    sha256
});

#[derive(Serialize)]
pub(super) struct Response {
    items: Vec<Item>,
    has_more: bool,
    dataset: Dataset,
}

pub(super) async fn search(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Query(params): Query<Search>,
) -> Result<Json<Response>, AppError> {
    super::owned_merchant(&state, &customer).await?;
    let category = params.category.as_deref().unwrap_or("01");
    if category != "01" && category != "all" {
        return Err(AppError::BadRequest("category must be 01 or all".into()));
    }
    let query = params.query.trim();
    if query.chars().count() > 200 {
        return Err(AppError::BadRequest("query is too long".into()));
    }
    let limit = params.limit.unwrap_or(8).clamp(1, 100);
    let offset = params.offset.unwrap_or(0);
    if !(0..=1_000_000).contains(&offset) {
        return Err(AppError::BadRequest(
            "offset must be between 0 and 1000000".into(),
        ));
    }
    let escaped = escape_like(query);
    let pattern = format!("%{escaped}%");
    let client = state.pool.get().await?;
    let dataset = crate::db::fetch_optional::<Dataset>(&client,
        "SELECT name, dataset_date AS date, record_count, source_url, sha256 FROM neurovector.food_reference_dataset WHERE singleton", &[])
        .await?.ok_or_else(|| AppError::ServiceUnavailable("Food reference dataset is not loaded; run the food reference loader".into()))?;
    let mut items = crate::db::fetch_all::<Item>(&client, r#"
        SELECT food_code, name, manufacturer, major_category_code, major_category, representative_name,
               subcategory, food_weight, weight_grams, nutrition_basis, report_number, source_name, reference_date
        FROM neurovector.food_reference
        WHERE ($1 = 'all' OR major_category_code = $1)
          AND (name ILIKE $2 ESCAPE '\' OR manufacturer ILIKE $2 ESCAPE '\'
               OR food_code ILIKE $2 ESCAPE '\' OR report_number ILIKE $2 ESCAPE '\')
        ORDER BY name, food_code LIMIT $3 OFFSET $4
    "#, &[&category, &pattern, &(limit + 1), &offset]).await?;
    let has_more = items.len()
        > usize::try_from(limit).map_err(|_| AppError::BadRequest("invalid limit".into()))?;
    if has_more {
        items.pop();
    }
    Ok(Json(Response {
        items,
        has_more,
        dataset,
    }))
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_like_metacharacters_when_search_is_literal() {
        assert_eq!(super::escape_like(r"과자%_\"), r"과자\%\_\\");
    }
}
