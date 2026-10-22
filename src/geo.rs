use serde::{Deserialize, Serialize};

/// 위도/경도/고도 기준의 경로 지점.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Waypoint {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub altitude_m: f64,
}

pub type Point = (f64, f64);

const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// 두 위경도 사이의 대권 거리(m).
pub fn haversine_m(a: Point, b: Point) -> f64 {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// 경로 전체를 감싸는 bounding box. padding_m 만큼 여유를 둔다.
pub fn bounding_box(points: &[Point], padding_m: f64) -> (f64, f64, f64, f64) {
    let min_lat = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let max_lat = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let min_lon = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let max_lon = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);

    let lat_pad = padding_m / 111_320.0;
    let mid_lat = ((min_lat + max_lat) / 2.0).to_radians();
    let lon_pad = padding_m / (111_320.0 * mid_lat.cos().abs().max(1e-6));

    (
        (min_lat - lat_pad).max(-90.0),
        (max_lat + lat_pad).min(90.0),
        (min_lon - lon_pad).max(-180.0),
        (max_lon + lon_pad).min(180.0),
    )
}

/// 점 p 에서 선분 a-b 까지의 최단 거리(m). 국소 평면 근사를 사용한다.
pub fn point_to_segment_m(p: Point, a: Point, b: Point) -> f64 {
    let lat0 = p.0.to_radians();
    let m_per_deg_lat = 111_320.0;
    let m_per_deg_lon = 111_320.0 * lat0.cos().abs().max(1e-6);

    let to_xy = |q: Point| ((q.1 - p.1) * m_per_deg_lon, (q.0 - p.0) * m_per_deg_lat);
    let (ax, ay) = to_xy(a);
    let (bx, by) = to_xy(b);
    let (px, py) = (0.0, 0.0);

    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;

    let t = if len_sq <= f64::EPSILON {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0)
    };

    let cx = ax + t * dx;
    let cy = ay + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// 점에서 경로(폴리라인)까지의 최단 거리(m).
pub fn point_to_route_m(p: Point, points: &[Point]) -> f64 {
    if points.is_empty() {
        return f64::INFINITY;
    }
    if points.len() == 1 {
        return haversine_m(p, points[0]);
    }
    points
        .windows(2)
        .map(|pair| point_to_segment_m(p, pair[0], pair[1]))
        .fold(f64::INFINITY, f64::min)
}

/// 위경도 기준 ray-casting. 경계는 내부로 취급하지 않는다.
pub fn point_in_polygon(lat: f64, lon: f64, polygon: &[Waypoint]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (yi, xi) = (polygon[i].lat, polygon[i].lon);
        let (yj, xj) = (polygon[j].lat, polygon[j].lon);
        let intersects =
            ((yi > lat) != (yj > lat)) && (lon < (xj - xi) * (lat - yi) / (yj - yi) + xi);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 경로를 max_step_m 간격으로 샘플링한다.
#[allow(dead_code)]
pub fn sample_route(points: &[Point], max_step_m: f64) -> Vec<Point> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut sampled = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let dist = haversine_m(a, b);
        let steps = ((dist / max_step_m).ceil() as usize).max(1);
        for step in 0..steps {
            let t = step as f64 / steps as f64;
            sampled.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    if let Some(last) = points.last() {
        sampled.push(*last);
    }
    sampled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_is_reasonable() {
        let seoul = (37.5665, 126.9780);
        let busan = (35.1796, 129.0756);
        let distance = haversine_m(seoul, busan);
        assert!((distance - 325_000.0).abs() < 20_000.0, "{distance}");
    }

    #[test]
    fn point_in_polygon_detects_inside_and_outside() {
        let polygon = vec![
            Waypoint {
                lat: 0.0,
                lon: 0.0,
                altitude_m: 10.0,
            },
            Waypoint {
                lat: 0.0,
                lon: 1.0,
                altitude_m: 10.0,
            },
            Waypoint {
                lat: 1.0,
                lon: 1.0,
                altitude_m: 10.0,
            },
            Waypoint {
                lat: 1.0,
                lon: 0.0,
                altitude_m: 10.0,
            },
        ];
        assert!(point_in_polygon(0.5, 0.5, &polygon));
        assert!(!point_in_polygon(1.5, 0.5, &polygon));
    }

    #[test]
    fn point_to_segment_handles_endpoints() {
        let distance = point_to_segment_m((0.0, 0.5), (0.0, 0.0), (0.0, 1.0));
        assert!(distance < 1.0, "{distance}");

        let outside = point_to_segment_m((1.0, 0.5), (0.0, 0.0), (0.0, 1.0));
        assert!(outside > 100_000.0, "{outside}");
    }

    #[test]
    fn sampling_includes_start_and_end() {
        let samples = sample_route(&[(0.0, 0.0), (0.0, 0.01)], 100.0);
        assert!(samples.len() > 2);
        assert_eq!(samples.first(), Some(&(0.0, 0.0)));
        assert_eq!(samples.last(), Some(&(0.0, 0.01)));
    }
}
