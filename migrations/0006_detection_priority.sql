-- 감지 객체 위험 우선순위: 1=얼굴/신체/동물, 2=차량/사물, 3=나뭇잎 등 저위험 자연물
ALTER TABLE neurovector.detections
    ADD COLUMN IF NOT EXISTS priority_level smallint NOT NULL DEFAULT 2;

ALTER TABLE neurovector.detections
    DROP CONSTRAINT IF EXISTS detections_priority_level_range;

ALTER TABLE neurovector.detections
    ADD CONSTRAINT detections_priority_level_range
    CHECK (priority_level BETWEEN 1 AND 3);
