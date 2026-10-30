CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS neurovector.food_reference (
    food_code text PRIMARY KEY CHECK (food_code <> ''),
    name text NOT NULL,
    major_category_code text NOT NULL,
    major_category text NOT NULL,
    representative_name text NOT NULL,
    subcategory text NOT NULL,
    manufacturer text NOT NULL,
    importer text NOT NULL,
    distributor text NOT NULL,
    imported boolean,
    origin_country text NOT NULL,
    food_weight text NOT NULL,
    weight_grams integer CHECK (weight_grams IS NULL OR weight_grams > 0),
    nutrition_basis text NOT NULL,
    report_number text NOT NULL,
    source_name text NOT NULL,
    generated_date date,
    reference_date date,
    source_data jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS food_reference_name_trgm ON neurovector.food_reference USING gin (name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS food_reference_manufacturer_trgm ON neurovector.food_reference USING gin (manufacturer gin_trgm_ops);
CREATE INDEX IF NOT EXISTS food_reference_code_trgm ON neurovector.food_reference USING gin (food_code gin_trgm_ops);
CREATE INDEX IF NOT EXISTS food_reference_report_trgm ON neurovector.food_reference USING gin (report_number gin_trgm_ops);
CREATE INDEX IF NOT EXISTS food_reference_category_order ON neurovector.food_reference (major_category_code, name, food_code);
CREATE TABLE IF NOT EXISTS neurovector.food_reference_dataset (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    name text NOT NULL,
    dataset_date date NOT NULL,
    record_count bigint NOT NULL CHECK (record_count > 0),
    source_url text NOT NULL,
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$')
);
