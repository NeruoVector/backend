CREATE TABLE IF NOT EXISTS neurovector.customer_users (
 id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
 username text NOT NULL UNIQUE,
 password_hash text NOT NULL,
 name text NOT NULL,
 phone text NOT NULL UNIQUE,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS neurovector.customer_sessions (
 session_hash text PRIMARY KEY,
 user_id uuid NOT NULL REFERENCES neurovector.customer_users(id) ON DELETE CASCADE,
 expires_at timestamptz NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS customer_sessions_expiry_idx ON neurovector.customer_sessions(expires_at);
ALTER TABLE neurovector.orders ADD COLUMN IF NOT EXISTS customer_user_id uuid REFERENCES neurovector.customer_users(id);
CREATE INDEX IF NOT EXISTS orders_customer_user_idx ON neurovector.orders(customer_user_id, created_at DESC);
ALTER TABLE neurovector.orders ADD COLUMN IF NOT EXISTS pickup_code text;
UPDATE neurovector.orders SET pickup_code = lpad((('x' || substr(replace(uuid_generate_v4()::text,'-',''),1,7))::bit(28)::int % 10000)::text,4,'0') WHERE pickup_code IS NULL;
ALTER TABLE neurovector.orders ALTER COLUMN pickup_code SET NOT NULL;
DO $$ BEGIN
 IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname='orders_pickup_code_format' AND conrelid='neurovector.orders'::regclass) THEN
  ALTER TABLE neurovector.orders ADD CONSTRAINT orders_pickup_code_format CHECK (pickup_code ~ '^[0-9]{4}$');
 END IF;
END $$;
