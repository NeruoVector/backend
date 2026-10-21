-- 단안 카메라 거리 추정값 (미터). 추정 불가/구버전 이벤트는 NULL.
ALTER TABLE neurovector.detections
    ADD COLUMN IF NOT EXISTS distance_m double precision;

ALTER TABLE neurovector.detections
    DROP CONSTRAINT IF EXISTS detections_distance_m_nonnegative;

ALTER TABLE neurovector.detections
    ADD CONSTRAINT detections_distance_m_nonnegative
    CHECK (distance_m IS NULL OR distance_m >= 0);
