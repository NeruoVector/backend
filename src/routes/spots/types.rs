use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct Spot {
    pub id: Uuid,
    pub label: String,
    pub delivery_address: String,
    pub delivery_detail: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub room_number: Option<String>,
    pub grade: Option<i32>,
    pub classroom: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
crate::db::row!(Spot {
    id,
    label,
    delivery_address,
    delivery_detail,
    latitude,
    longitude,
    room_number,
    grade,
    classroom,
    created_at,
    updated_at
});

#[derive(Debug, Deserialize)]
pub struct SpotInput {
    pub label: String,
    pub delivery_address: String,
    #[serde(default)]
    pub delivery_detail: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub room_number: Option<String>,
    #[serde(default)]
    pub grade: Option<i32>,
    #[serde(default)]
    pub classroom: Option<String>,
}

pub(super) struct SpotFields {
    pub(super) label: String,
    pub(super) delivery_address: String,
    pub(super) delivery_detail: Option<String>,
    pub(super) latitude: f64,
    pub(super) longitude: f64,
    pub(super) room_number: Option<String>,
    pub(super) grade: Option<i32>,
    pub(super) classroom: Option<String>,
}

impl SpotInput {
    pub(super) fn into_fields(self) -> Result<SpotFields, AppError> {
        let label = self.label.trim();
        let delivery_address = self.delivery_address.trim();
        if label.is_empty() || label.chars().count() > 60 {
            return Err(AppError::BadRequest(
                "label must contain between 1 and 60 characters".into(),
            ));
        }
        if delivery_address.is_empty() || delivery_address.chars().count() > 300 {
            return Err(AppError::BadRequest(
                "delivery_address must contain between 1 and 300 characters".into(),
            ));
        }
        if !self.latitude.is_finite() || !(-90.0..=90.0).contains(&self.latitude) {
            return Err(AppError::BadRequest(
                "latitude must be finite and between -90 and 90".into(),
            ));
        }
        if !self.longitude.is_finite() || !(-180.0..=180.0).contains(&self.longitude) {
            return Err(AppError::BadRequest(
                "longitude must be finite and between -180 and 180".into(),
            ));
        }

        let room_number = self.room_number.map(|value| value.trim().to_owned());
        let classroom = self.classroom.map(|value| value.trim().to_owned());
        let outdoor_location = room_number.is_none() && self.grade.is_none() && classroom.is_none();
        let room_location = room_number
            .as_ref()
            .is_some_and(|value| !value.is_empty() && value.chars().count() <= 40)
            && self.grade.is_none()
            && classroom.is_none();
        let classroom_location = room_number.is_none()
            && self.grade.is_some_and(|value| (1..=12).contains(&value))
            && classroom
                .as_ref()
                .is_some_and(|value| !value.is_empty() && value.chars().count() <= 40);
        if !outdoor_location && !room_location && !classroom_location {
            return Err(AppError::BadRequest(
                "provide either room_number or both grade and classroom".into(),
            ));
        }

        let delivery_detail = self.delivery_detail.and_then(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        });
        if delivery_detail
            .as_ref()
            .is_some_and(|value| value.chars().count() > 300)
        {
            return Err(AppError::BadRequest(
                "delivery_detail must contain at most 300 characters".into(),
            ));
        }

        Ok(SpotFields {
            label: label.to_owned(),
            delivery_address: delivery_address.to_owned(),
            delivery_detail,
            latitude: self.latitude,
            longitude: self.longitude,
            room_number,
            grade: self.grade,
            classroom,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> SpotInput {
        SpotInput {
            label: " Home ".into(),
            delivery_address: "Seoul".into(),
            delivery_detail: None,
            latitude: 37.5,
            longitude: 127.0,
            room_number: Some(" 101 ".into()),
            grade: None,
            classroom: None,
        }
    }

    #[test]
    fn spot_accepts_room_or_grade_classroom_and_normalizes_text() {
        // Given a room spot and a classroom spot, When parsing both, Then the valid indoor form is normalized.
        let fields = input().into_fields().expect("valid room spot");
        assert_eq!(fields.label, "Home");
        assert_eq!(fields.room_number.as_deref(), Some("101"));

        let mut classroom = input();
        classroom.room_number = None;
        classroom.grade = Some(3);
        classroom.classroom = Some(" 3-2 ".into());
        let fields = classroom.into_fields().expect("valid classroom spot");
        assert_eq!(fields.grade, Some(3));
        assert_eq!(fields.classroom.as_deref(), Some("3-2"));
    }

    #[test]
    fn spot_rejects_ambiguous_indoor_location_and_non_finite_coordinates() {
        // Given ambiguous indoor fields or a non-finite coordinate, When parsing, Then the boundary rejects the spot.
        let mut ambiguous = input();
        ambiguous.grade = Some(2);
        ambiguous.classroom = Some("2-1".into());
        assert!(ambiguous.into_fields().is_err());

        let mut invalid = input();
        invalid.longitude = f64::NAN;
        assert!(invalid.into_fields().is_err());
    }

    #[test]
    fn outdoor_spot_accepts_no_indoor_fields() {
        // Given an outdoor GPS location, When no indoor detail is supplied, Then the Spot remains valid.
        let mut outdoor = input();
        outdoor.room_number = None;
        let fields = outdoor.into_fields().expect("valid outdoor spot");

        assert!(fields.room_number.is_none());
        assert!(fields.grade.is_none());
        assert!(fields.classroom.is_none());
    }

    #[test]
    fn spot_rejects_partial_indoor_classroom() {
        // Given only a grade without classroom, When parsing the Spot, Then indoor validation rejects it.
        let mut partial = input();
        partial.room_number = None;
        partial.grade = Some(4);

        assert!(partial.into_fields().is_err());
    }
}
