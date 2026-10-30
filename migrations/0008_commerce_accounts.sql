CREATE TABLE IF NOT EXISTS neurovector.customers (
 id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
 provider text NOT NULL CHECK(provider IN ('google','kakao','naver')),
 subject text NOT NULL, name text NOT NULL,
 merchant_enabled boolean NOT NULL DEFAULT false,
 UNIQUE(provider, subject)
);
CREATE TABLE IF NOT EXISTS neurovector.customer_sessions (
 token_hash text PRIMARY KEY, customer_id uuid NOT NULL REFERENCES neurovector.customers(id) ON DELETE CASCADE,
 expires_at timestamptz NOT NULL
);
CREATE TABLE IF NOT EXISTS neurovector.oauth_states (
 token_hash text PRIMARY KEY, provider text NOT NULL, expires_at timestamptz NOT NULL
);
CREATE TABLE IF NOT EXISTS neurovector.student_verifications (
 customer_id uuid PRIMARY KEY REFERENCES neurovector.customers(id) ON DELETE CASCADE,
 school text NOT NULL CHECK(length(school) BETWEEN 1 AND 100),
 student_number text NOT NULL CHECK(length(student_number) BETWEEN 1 AND 40),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','approved','rejected')),
 evidence bytea, content_type text NOT NULL, reason text,
 submitted_at timestamptz NOT NULL DEFAULT now(), reviewed_at timestamptz
);
CREATE TABLE IF NOT EXISTS neurovector.merchants (
 id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
 owner_id uuid NOT NULL UNIQUE REFERENCES neurovector.customers(id),
 name text NOT NULL CHECK(length(name) BETWEEN 1 AND 100),
 description text NOT NULL DEFAULT '', cover_url text NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS neurovector.product_categories (
 id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
 merchant_id uuid NOT NULL REFERENCES neurovector.merchants(id),
 name text NOT NULL CHECK(length(name) BETWEEN 1 AND 100),
 UNIQUE(merchant_id,name), UNIQUE(id,merchant_id)
);
CREATE TABLE IF NOT EXISTS neurovector.product_details (
 item_id uuid PRIMARY KEY REFERENCES neurovector.inventory_items(id) ON DELETE CASCADE,
 merchant_id uuid NOT NULL REFERENCES neurovector.merchants(id),
 category_id uuid NOT NULL,
 image_url text NOT NULL DEFAULT '',
 weight_grams integer NOT NULL DEFAULT 0,
 FOREIGN KEY(category_id,merchant_id) REFERENCES neurovector.product_categories(id,merchant_id)
);
ALTER TABLE neurovector.product_details
 ADD COLUMN IF NOT EXISTS weight_grams integer NOT NULL DEFAULT 0;
DO $$ BEGIN
 IF NOT EXISTS (
  SELECT 1 FROM pg_constraint
  WHERE conname='product_details_weight_nonnegative'
    AND conrelid='neurovector.product_details'::regclass
 ) THEN
  ALTER TABLE neurovector.product_details
   ADD CONSTRAINT product_details_weight_nonnegative CHECK (weight_grams >= 0);
 END IF;
END $$;
CREATE TABLE IF NOT EXISTS neurovector.order_handoffs (
 order_id uuid PRIMARY KEY REFERENCES neurovector.orders(id) ON DELETE CASCADE,
 customer_id uuid NOT NULL REFERENCES neurovector.customers(id),
 pin_hash text NOT NULL, attempts integer NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 5),
 consumed_at timestamptz
);
-- Store the catalog weight bought at checkout so later product edits cannot
-- change dispatch capacity calculations. NULL is reserved for legacy items.
ALTER TABLE neurovector.order_items
 ADD COLUMN IF NOT EXISTS weight_grams integer
 CHECK (weight_grams IS NULL OR weight_grams > 0);
-- Both the public status API and simulation must pass the password gate.
CREATE OR REPLACE FUNCTION neurovector.guard_order_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.status = 'delivered' AND EXISTS (
  SELECT 1 FROM neurovector.order_handoffs
  WHERE order_id=NEW.id
    AND (consumed_at IS NULL OR OLD.status <> 'in_transit')
  ) THEN
  RAISE EXCEPTION USING
   ERRCODE='23514', CONSTRAINT='order_handoff_guard',
   MESSAGE='handoff PIN verification required';
 END IF;
 RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS order_handoff_guard ON neurovector.orders;
CREATE TRIGGER order_handoff_guard BEFORE UPDATE OF status ON neurovector.orders
 FOR EACH ROW EXECUTE FUNCTION neurovector.guard_order_handoff();
CREATE OR REPLACE FUNCTION neurovector.guard_delivery_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.status = 'completed' AND EXISTS (
  SELECT 1 FROM neurovector.orders o JOIN neurovector.order_handoffs h ON h.order_id=o.id
  WHERE o.delivery_id=NEW.id
    AND o.status <> 'cancelled'
    AND (h.consumed_at IS NULL OR o.status <> 'delivered')
  ) THEN
  RAISE EXCEPTION USING
   ERRCODE='23514', CONSTRAINT='delivery_handoff_guard',
   MESSAGE='handoff PIN verification required';
 END IF;
 RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS delivery_handoff_guard ON neurovector.deliveries;
CREATE TRIGGER delivery_handoff_guard BEFORE UPDATE OF status ON neurovector.deliveries
 FOR EACH ROW EXECUTE FUNCTION neurovector.guard_delivery_handoff();
