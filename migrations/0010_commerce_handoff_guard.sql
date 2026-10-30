-- Update databases that already ran 0008 with the stricter final-delivery
-- guard. Cancelled siblings do not block completion, while every active PIN
-- order must still have a consumed PIN and a delivered status.
CREATE OR REPLACE FUNCTION neurovector.guard_delivery_handoff()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
 IF NEW.status = 'completed' AND EXISTS (
  SELECT 1
  FROM neurovector.orders o
  JOIN neurovector.order_handoffs h ON h.order_id = o.id
  WHERE o.delivery_id = NEW.id
    AND o.status <> 'cancelled'
    AND (h.consumed_at IS NULL OR o.status <> 'delivered')
 ) THEN
  RAISE EXCEPTION USING
   ERRCODE = '23514',
   CONSTRAINT = 'delivery_handoff_guard',
   MESSAGE = 'handoff PIN verification required';
 END IF;
 RETURN NEW;
END;
$$;
