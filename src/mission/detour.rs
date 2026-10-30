use super::*;

pub(super) struct PreparedZones {
    polygons: Vec<(Vec<Waypoint>, Option<f64>)>,
}

impl PreparedZones {
    pub(super) fn new(zones: &[NoFlyZoneRow]) -> Result<Self, AppError> {
        if zones.len() > 64 {
            return Err(bad("planning supports at most 64 active no-fly zones"));
        }
        let mut polygons = Vec::with_capacity(zones.len());
        let mut vertices = 0;
        for z in zones {
            let p: Vec<Waypoint> = serde_json::from_value(z.polygon.clone())
                .map_err(|_| bad("invalid active no-fly polygon"))?;
            if !(3..=256).contains(&p.len())
                || p.iter().any(|w| validate_lat_lon(w.lat, w.lon).is_err())
            {
                return Err(bad("invalid active no-fly polygon"));
            }
            vertices += p.len();
            polygons.push((p, z.max_altitude_m));
        }
        if vertices > 126 {
            return Err(bad(
                "visibility graph supports at most 126 active polygon vertices",
            ));
        }
        Ok(Self { polygons })
    }

    /// Check each edge against all restricted polygons before selecting a detour.
    pub(super) fn generate_detour(
        &self,
        state: &AppState,
        start: &Waypoint,
        end: &Waypoint,
        window: Option<&WindowTarget>,
    ) -> Option<Vec<Waypoint>> {
        let polygons = &self.polygons;
        let goal = window.map_or_else(
            || end.clone(),
            |w| offset(&w.window, w.outward_bearing_deg, w.approach_distance_m),
        );
        let exterior = |p: &Waypoint| -> bool {
            window.is_none_or(|w| {
                let b = w.outward_bearing_deg.to_radians();
                let n = (p.lat - w.window.lat).to_radians() * 6_371_000.0;
                let e = (p.lon - w.window.lon).to_radians()
                    * 6_371_000.0
                    * w.window.lat.to_radians().cos();
                n * b.cos() + e * b.sin() >= w.standoff_m - 0.05
            })
        };
        let clear = |a: &Waypoint, b: &Waypoint| -> bool {
            (a.lon - b.lon).abs() <= 180.0
                && exterior(a)
                && exterior(b)
                && crate::geo::haversine_m((a.lat, a.lon), (b.lat, b.lon))
                    .hypot(b.altitude_m - a.altitude_m)
                    <= state.route_max_leg_m
                && polygons
                    .iter()
                    .all(|(p, c)| !crate::routes::route_plans::segment_hits_zone(a, b, p, *c))
        };
        if clear(start, &goal) {
            return None;
        }
        let mut nodes = vec![start.clone(), goal.clone()];
        for (polygon, _) in polygons {
            let center_lat = polygon.iter().map(|w| w.lat).sum::<f64>() / polygon.len() as f64;
            let center_lon = polygon.iter().map(|w| w.lon).sum::<f64>() / polygon.len() as f64;
            for vertex in polygon {
                let bearing = ((vertex.lon - center_lon) * center_lat.to_radians().cos())
                    .atan2(vertex.lat - center_lat)
                    .to_degrees();
                let mut p = offset(vertex, bearing, 8.0);
                p.altitude_m = goal.altitude_m;
                if exterior(&p) {
                    nodes.push(p);
                }
            }
        }
        let n = nodes.len();
        let mut distance = vec![f64::INFINITY; n];
        let mut previous = vec![None; n];
        let mut visited = vec![false; n];
        distance[0] = 0.0;
        for _ in 0..n {
            let Some(u) = (0..n)
                .filter(|i| !visited[*i])
                .min_by(|a, b| distance[*a].total_cmp(&distance[*b]))
            else {
                break;
            };
            if !distance[u].is_finite() || u == 1 {
                break;
            }
            visited[u] = true;
            for v in 0..n {
                if visited[v] || !clear(&nodes[u], &nodes[v]) {
                    continue;
                }
                let next = distance[u]
                    + crate::geo::haversine_m(
                        (nodes[u].lat, nodes[u].lon),
                        (nodes[v].lat, nodes[v].lon),
                    )
                    .hypot(nodes[v].altitude_m - nodes[u].altitude_m);
                if next < distance[v] {
                    distance[v] = next;
                    previous[v] = Some(u);
                }
            }
        }
        if !distance[1].is_finite() {
            return None;
        }
        let mut indices = vec![1];
        let mut cursor = 1;
        while cursor != 0 {
            let parent = previous[cursor]?;
            indices.push(parent);
            cursor = parent;
        }
        indices.reverse();
        let mut path: Vec<_> = indices.into_iter().map(|i| nodes[i].clone()).collect();
        // Window assembly appends the exact outside approach and hover. Remove graph goal first.
        if window.is_some() {
            path.pop();
            path.push(end.clone());
        }
        Some(path)
    }
}

#[cfg(test)]
pub(super) fn generate_detour(
    state: &AppState,
    start: &Waypoint,
    end: &Waypoint,
    zones: &[NoFlyZoneRow],
    window: Option<&WindowTarget>,
) -> Result<Option<Vec<Waypoint>>, AppError> {
    Ok(PreparedZones::new(zones)?.generate_detour(state, start, end, window))
}
