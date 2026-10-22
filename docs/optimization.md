# Backend optimization notes

These changes reduce database round trips and repeated Redis connection setup, and
move password computation off the asynchronous request executor. The figures below
come from the query structure; production latency and throughput have not been
benchmarked. No automated tests were added or run for this optimization, and no
migrations were applied to a database.

## Order creation

The synchronous API and Kafka worker share `create_order_in_tx`. For an order with
N distinct items, successful creation now executes four SQL statements:

1. Fetch and lock all requested inventory rows using `ANY(uuid[])`, ordered by ID.
2. Decrement stock using paired UUID and quantity arrays with `UNNEST`.
3. Insert the order.
4. Insert all order items using UUID, quantity, price and total arrays with `UNNEST`.

Previously this required **3N + 1** statements: N inventory reads, N stock updates,
one order insert and N item inserts. For ten items, this is **31 → 4** statements.
The count excludes BEGIN/COMMIT, the optional delivery-existence check (one extra
statement in both versions), and cache or event operations outside creation.

Inventory rows are locked in UUID order before validation and updates. Validation
still follows request order, retaining existing missing-item, inactive-item,
insufficient-stock and mixed-currency errors. Line multiplication and total
addition retain checked arithmetic. The existing transaction rolls back all
changes on failure. Returned items are reconstructed in request order because
`INSERT ... RETURNING` does not promise an order. Both callers retain the existing
validation that rejects duplicate IDs and invalid quantities.

Cancellation and refund stock restoration also read items in UUID order before
updating inventory. This aligns inventory lock acquisition with order creation;
restoration itself still uses one update per item.

## Redis connection reuse and deadlines

`RedisStore` clones share a lazily initialized `ConnectionManager`. Redis is not
contacted merely to construct application state, and failed initialization can be
retried. Commands reuse the multiplexed manager instead of opening a connection
for each operation.

`REDIS_TIMEOUT_MS` defaults to **500 ms** and is clamped to at least 1 ms. The
`execute` deadline covers the whole operation, including initial connection,
manager reconnection waits and command responses. Connection and response
timeouts are also configured on the manager. This is a Redis operation budget,
not a deadline for the whole HTTP request.

Related cache prefixes can be invalidated in a single cursor-based SCAN traversal
with `COUNT 200` and batched `UNLINK`. This means one traversal, not one SCAN
command: each returned cursor page requires another command. Multiple prefixes
are filtered within the cache namespace. Order mutations combine `orders:`,
`admin:orders:` and `inventory:` in this traversal. UNLINK removes keys while
allowing Redis to reclaim value memory asynchronously.

Cache access and invalidation remain best effort. The deadline covers an entire
prefix scan, so a large or slow scan can time out after deleting only some keys.
A timeout does not roll back already executed Redis commands. Remaining cached
values expire through their configured TTL; reads may return stale values until
then. SCAN also does not provide a snapshot of concurrent cache writes. Monitor
invalidation warnings and size the TTL and Redis budget for the deployment.

## Password work and readiness

Argon2 hashing and verification run in `spawn_blocking`. A process-local semaphore
allows at most **two** password computations concurrently. The blocking task owns
the permit until completion, including when its HTTP waiter is cancelled. Waiting
requests await a permit asynchronously; this limit does not impose a request
queue length or login timeout.

Readiness probes PostgreSQL and Redis concurrently. PostgreSQL has a **2-second**
probe deadline; Redis uses the configured `REDIS_TIMEOUT_MS` operation deadline.
The endpoint reports 503 when either probe fails. Concurrent probing avoids
adding the two probe durations together, subject to normal runtime scheduling.

## Delivery queries and dispatch compatibility

Delivery overlap detection uses `EXISTS` with a direct `scheduled_at` range rather
than counting rows through an `abs(extract(epoch ...))` expression. The open range
preserves the existing rule: scheduled or dispatched deliveries strictly less
than ten minutes apart conflict; exactly ten minutes remains allowed. The
predicates can use the existing `(drone_id, scheduled_at)` index. This query
rewrite does not add a database exclusion constraint or make the existing
check-then-insert sequence atomic across concurrent requests.

Status updates compare the previously read status in the UPDATE predicate and
return a conflict if another request changed it. Transitions to `dispatched` and
`in_flight` additionally require a verified mission plan for the delivery's route
and drone. A legacy route alone no longer satisfies dispatch requirements.
Create a mission through `POST /api/v1/routes/plan`, have the drone independently
validate its plan, and submit the report through
`POST /api/v1/routes/plans/{id}/cross-check` before dispatch. Expired plans or
changed flight constraints/no-fly zones require a fresh plan and cross-check.
The mission-plan schema migration must be deployed separately; this optimization
work did not apply it.
