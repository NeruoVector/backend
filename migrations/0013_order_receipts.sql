CREATE TABLE IF NOT EXISTS neurovector.order_receipts (
    order_id uuid PRIMARY KEY REFERENCES neurovector.orders(id) ON DELETE CASCADE,
    received_at timestamptz NOT NULL DEFAULT now()
);
