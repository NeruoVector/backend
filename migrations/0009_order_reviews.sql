-- One immutable review per paid order. Photos are persisted with the review.
CREATE TABLE IF NOT EXISTS neurovector.order_reviews (
    id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    order_id uuid NOT NULL UNIQUE REFERENCES neurovector.orders(id) ON DELETE CASCADE,
    rating smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    content text NOT NULL CHECK (char_length(btrim(content)) BETWEEN 1 AND 500),
    tags text[] NOT NULL DEFAULT '{}',
    photo_data bytea,
    photo_mime text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (cardinality(tags) <= 3),
    CHECK (tags <@ ARRAY['맛있어요', '배달이 빨라요', '포장이 깔끔해요']::text[]),
    CHECK ((photo_data IS NULL AND photo_mime IS NULL) OR
           (photo_data IS NOT NULL AND photo_mime IS NOT NULL AND photo_mime IN ('image/jpeg', 'image/png', 'image/webp')
            AND octet_length(photo_data) BETWEEN 1 AND 1048576))
);
CREATE INDEX IF NOT EXISTS order_reviews_created_idx
    ON neurovector.order_reviews (created_at DESC, id DESC);
