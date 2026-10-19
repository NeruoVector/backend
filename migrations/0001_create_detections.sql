-- 장애물 감지 이벤트 저장 테이블
CREATE TABLE IF NOT EXISTS neurovector.detections (
    id           uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    drone_id     text        NOT NULL,
    label        text        NOT NULL,
    confidence   real        NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
    latitude     double precision,
    longitude    double precision,
    detected_at  timestamptz NOT NULL DEFAULT now(),
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS detections_detected_at_idx
    ON neurovector.detections (detected_at DESC);

CREATE INDEX IF NOT EXISTS detections_drone_id_idx
    ON neurovector.detections (drone_id);
