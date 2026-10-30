use super::*;

pub(super) async fn revalidate(
    state: &AppState,
    plan: &MissionPlan,
    drone: &str,
) -> Result<(PlanPayload, RouteValidation), AppError> {
    let connection = state.pool.get().await?;
    revalidate_with_client(state, &connection, plan, drone).await
}

pub(super) async fn revalidate_with_client(
    state: &AppState,
    client: &impl deadpool_postgres::GenericClient,
    plan: &MissionPlan,
    drone: &str,
) -> Result<(PlanPayload, RouteValidation), AppError> {
    if plan.expires_at <= Utc::now() || plan.drone_id != drone {
        return Err(bad("mission expired or assigned to another drone"));
    }
    if !plan.plan_text.starts_with("NVPLAN2 ") {
        return Err(bad("legacy mission requires replanning"));
    }
    if fnv(&plan.plan_text) != plan.route_fingerprint {
        return Err(bad("stored mission fingerprint mismatch"));
    }
    let p: PlanPayload =
        serde_json::from_value(plan.payload.clone()).map_err(|e| bad(e.to_string()))?;
    if plan_text(plan.id, &plan.drone_id, plan.expires_at, &p)? != plan.plan_text {
        return Err(bad("stored mission payload changed"));
    }
    if p.required_stops.len() < 2
        || p.candidates.iter().any(|candidate| {
            !contains_required(&candidate.waypoints, &p.required_stops)
                || candidate
                    .waypoints
                    .first()
                    .zip(p.required_stops.first())
                    .is_none_or(|(a, b)| !same(a, b))
                || candidate
                    .waypoints
                    .last()
                    .zip(p.required_stops.last())
                    .is_none_or(|(a, b)| !same(a, b))
        })
    {
        return Err(bad("mission candidates omit required stops"));
    }
    let r = p
        .candidates
        .get(p.selected_index)
        .ok_or_else(|| bad("invalid selected candidate"))?;
    let zones = crate::db::fetch_all::<NoFlyZoneRow>(
        client,
        "SELECT id, name, polygon, max_altitude_m FROM neurovector.no_fly_zones WHERE active = true ORDER BY id",
        &[],
    )
    .await?;
    if serde_json::to_value(&zones).ok() != serde_json::to_value(&p.zones).ok() {
        return Err(bad("no-fly zones changed; create a fresh mission plan"));
    }
    let current = constraints(state, r, p.constraints.hold_seconds);
    if serde_json::to_value(current).ok() != serde_json::to_value(&p.constraints).ok() {
        return Err(bad(
            "flight constraints changed; create a fresh mission plan",
        ));
    }
    let validations: Vec<_> = p
        .candidates
        .iter()
        .map(|r| validate_route_with_zones(state, r, &zones, p.constraints.hold_seconds))
        .collect();
    let best = validations
        .iter()
        .enumerate()
        .filter(|(_, v)| v.valid)
        .min_by(|a, b| a.1.distance_m.total_cmp(&b.1.distance_m))
        .map(|(i, _)| i);
    if best != Some(p.selected_index) {
        return Err(bad(
            "selected candidate is no longer the minimum valid route",
        ));
    }
    let stored = crate::db::fetch_optional::<crate::routes::route_plans::Route>(
        client,
        "SELECT id, name, drone_id, waypoints, cruise_speed_mps, max_altitude_m, distance_m, estimated_duration_s, created_at, updated_at FROM neurovector.routes WHERE id = $1",
        &[&plan.route_id],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    if stored.drone_id.as_deref() != Some(drone)
        || stored.waypoints != serde_json::to_value(&r.waypoints).map_err(|e| bad(e.to_string()))?
        || Some(stored.cruise_speed_mps) != r.cruise_speed_mps
        || Some(stored.max_altitude_m) != r.max_altitude_m
    {
        return Err(bad("saved route no longer matches the verified mission"));
    }
    let v = validations[p.selected_index].clone();
    Ok((p, v))
}
pub(super) fn close(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 0.01_f64.max(b.abs() * 1e-6)
}
pub async fn verify_route_for_dispatch(
    state: &AppState,
    route_id: Uuid,
    drone_id: &str,
    delivery_id: Uuid,
) -> Result<(), AppError> {
    let plan = crate::db::fetch_optional::<MissionPlan>(
        &state.pool.get().await?,
        "SELECT * FROM neurovector.mission_plans WHERE route_id=$1",
        &[&(route_id)],
    )
    .await?
    .ok_or_else(|| {
        AppError::Conflict("create a cross-validated mission plan before dispatch".into())
    })?;
    if plan.status != "verified" || plan.verified_at.is_none() {
        return Err(bad("mission has not passed drone cross-check"));
    }
    let (payload, _) = revalidate(state, &plan, drone_id).await?;
    let connection = state.pool.get().await?;
    let stops = crate::db::fetch_all::<(Option<f64>, Option<f64>)>(&connection,
        "SELECT o.latitude, o.longitude FROM neurovector.delivery_stops s JOIN neurovector.orders o ON o.id=s.order_id WHERE s.delivery_id=$1 AND s.status IN ('scheduled','arrived','completed') ORDER BY s.stop_sequence", &[&delivery_id]).await?;
    let automatic = crate::db::scalar_one::<bool>(&connection, "SELECT schedule_source='automatic' FROM neurovector.deliveries WHERE id=$1 AND route_id=$2", &[&delivery_id, &route_id]).await?;
    if !delivery_stops_match(&payload, &stops, automatic) {
        return Err(bad("delivery customer stops do not match mission"));
    }
    Ok(())
}

pub(super) fn delivery_stops_match(
    payload: &PlanPayload,
    stops: &[(Option<f64>, Option<f64>)],
    automatic: bool,
) -> bool {
    if automatic && stops.is_empty() {
        return false;
    }
    let mut cursor = 1;
    for &(lat, lon) in stops {
        let (Some(lat), Some(lon)) = (lat, lon) else {
            return false;
        };
        let end = payload
            .required_stops
            .len()
            .saturating_sub(usize::from(automatic));
        let Some(index) = (cursor..end).find(|&i| {
            let point = &payload.required_stops[i];
            ((point.lat - lat).abs() < 1e-10 && (point.lon - lon).abs() < 1e-10)
                || (i + 1 == payload.required_stops.len()
                    && payload.window_target.as_ref().is_some_and(|w| {
                        (w.window.lat - lat).abs() < 1e-10 && (w.window.lon - lon).abs() < 1e-10
                    }))
        }) else {
            return false;
        };
        cursor = index + 1;
    }
    true
}
