-- One-time repair for codes initially derived from public order IDs.
CREATE TABLE IF NOT EXISTS neurovector.data_migrations (
 name text PRIMARY KEY,
 applied_at timestamptz NOT NULL DEFAULT now()
);
DO $$
BEGIN
 INSERT INTO neurovector.data_migrations(name)
  VALUES ('0011_randomize_legacy_pickup_codes')
 ON CONFLICT (name) DO NOTHING
 ;
 IF FOUND THEN
  UPDATE neurovector.orders
  SET pickup_code = lpad((('x' || substr(replace(uuid_generate_v4()::text,'-',''),1,7))::bit(28)::int % 10000)::text,4,'0')
  WHERE order_number ~ '^NV-[0-9]{8}-[0-9A-Fa-f]{8}$';
 END IF;
END $$;
