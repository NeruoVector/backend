# Delivery backend codemap

This note records the delivery modules before and after the requested decomposition. The baseline and final runs use the synthetic API fixture in `sim/logs/school-tests/report.json`; no physical flight was performed.

## Current module map

| Module | Responsibility and flow | Baseline size |
| --- | --- | ---: |
| `scheduling.rs` | `spawn_scheduler` ticks every five seconds; `run_once` owns the advisory transaction lock and orchestration. | 114 lines; originally 619 |
| `scheduling/planner.rs` | Pure batch planner, planner DTOs, route geometry/timing helpers and planner tests. Per-stop weight travels with the plan and is skipped during serialization. | 280 lines, including tests |
| `scheduling/repository.rs` | Transaction-bound candidate locking (`SKIP LOCKED`), existing assigned-order synchronization and active fleet availability reads. | 120 lines |
| `scheduling/persistence.rs` | Transaction-bound blocked events and atomic route/delivery/stop/order/event writes; stop, order and assignment-event writes now run once per batch. | 178 lines |
| `tracking.rs` | Router, owner-authenticated tracking handler and stable public DTO/re-export facade. | 182 lines; originally 672 |
| `tracking/types.rs` | Public tracking DTOs plus internal SQL row models. | 98 lines |
| `tracking/eta.rs` | ETA route-stop query and telemetry-based estimate. | 65 lines |
| `tracking/arrival.rs` | Arrival sample query, stop update and deduplicated notification event transaction. | 181 lines |
| `tracking/arrival_rules.rs` | Pure stationary hold-window, coordinate and freshness rules with regression tests. | 178 lines |
| `fleet.rs` | Admin-protected route registration, configuration reads/writes and deactivation. | 97 lines; originally 336 |
| `fleet/types.rs` | Fleet/payload DTOs, fleet input validation and validation tests. | 137 lines |
| `fleet/measurements.rs` | Trusted admin payload measurement transaction and per-stop tracking event writes. | 116 lines |
| `spots.rs` | Owner-authenticated Spot CRUD handlers and route registration. | 148 lines; originally 328 |
| `spots/types.rs` | Spot DTOs, normalized input validation and indoor/outdoor tests. | 187 lines |
| `deliveries.rs` | Existing delivery CRUD and mission verification, plus registered-fleet telemetry and automatic-delivery payload readiness checks before status transition; DB trigger repeats authoritative gates. | 476 lines |
| `flights.rs` | Existing flight APIs; status uses the latest server-received sample and requires fresh device and receive timestamps within the 5-second future-skew bound. | 3 freshness tests added |
| `0009_spots_dispatch.sql` | Spot/order destination schema, order-item weight snapshot column, fleet and stop/measurement/event tables, receive-ordered telemetry index, status triggers, dispatch guard and uniqueness needed by order/dedupe event writes. | migration |
| `0010_commerce_handoff_guard.sql` | Accounts-owned upgrade so a cancelled sibling does not block completion after remaining PIN stops are received. | applied by disposable runner |

`main.rs` starts the scheduler, and `routes/mod.rs` exposes these route facades; both are account/coordinator-owned and outside this refactor. Public route registration, customer/admin authorization, DTO names, JSON fields and migration handoff behavior remain stable.

## Behavior checks before refactor

- Pre-refactor baseline: `cargo test --offline --manifest-path backend/Cargo.toml`: 33 passed, 0 failed, 1 ignored.
- `SCHOOL_CAPTURE_SQL_LOG=1 SCHOOL_BACKEND_RUST_LOG='warn,sqlx::query=debug,neurovector_backend::routes::scheduling=info' python3 sim/scripts/test-school.py --with-backend`: passed 16 synthetic avoidance cases, two route cross-checks, and disposable PostgreSQL/backend API scenarios.
- The API fixture showed a verified route still rejects a device timestamp one hour old even when the receive time is new (HTTP 409); fresh telemetry permits the manual mission transition. Snapshot-weight scenarios assigned four 150g snapshot orders as two concurrent two-stop batches, while overweight and unweighed orders remained queued. Database dispatch gates rejected missing, zero, ten-minute-stale, and over-current-capacity measurements; a later safe 250g reweigh cleared a prior 400g reading at 300g configured capacity.
- The fixture mutates product metadata after inserting order-item snapshots, proving the scheduler reads the immutable order-item value. The accounts-owned checkout writes `order_items.weight_grams`; three real HTTP checkouts snapshot 100g unit weights, then the catalog changes to 999g, while a quantity-2 order still schedules the correct 400g batch total.
- The fixture also inserts an outdoor Spot with all indoor fields null and verifies an ambiguous room-plus-class insert violates the schema check.
- The upgrade fixture pre-creates the prior indoor-only Spot and order CHECK constraints before applying migration 0009; subsequent outdoor orders/Spots succeed, proving the migration replaces the old checks. The injected assignment-event error leaves the order ready with no delivery, stop or event; after removing the failure trigger the scheduler assigns exactly one stop and one event.
- The previous HTTP `estimated_weight_grams` INT4/INT8 decoder defect is fixed by selecting the stop weight as `bigint`. The handoff `ON CONFLICT(order_id,dedupe_key)` failure is fixed by explicitly creating that unique index for databases where the pre-existing table skipped its inline constraint. The order-status trigger completes stops and closes a multi-stop delivery only after its last active stop; accounts migration 0010 refreshes the canceled-sibling guard for existing databases.

## Measured scheduler operation baseline

The before and final after runs are retained in `sim/logs/school-tests/school-backend-sql-before-refactor.log` and `school-backend-sql-after-refactor.log`. The same initial eligible assignment transaction contains two batches and four stops. Before batching it has 23 SQL statements including the advisory lock and commit: four fixed setup/candidate/fleet statements, two blocked-event inserts, four route/delivery inserts, and twelve per-stop writes. After batching it has 17 statements, saving six round trips. The follow-on one-stop assignment is 11 statements versus the original 12 after the same bulk writes. SQLx-reported time summed across the latest captured initial assignment was 8.105ms before and 7.727ms after; repeated final samples ranged from 6.919ms to 9.688ms, so the stable measured gain is the lower query count, not a latency claim. The final simulator report and SQL trace are retained in `sim/logs/school-tests/report.json` and `school-backend-sql.log`.

The candidate `HashMap<Uuid, &ScheduleOrder>` was built once per scheduler tick, not once per batch. Carrying each stop's weight into its plan removed that map and per-stop lookup while making persistence data self-contained; no broader planner or arrival allocation rewrite was made.

The final simulator passes 16 avoidance scenarios, two route cross-checks, all 3 route rejections and disposable PostgreSQL/backend API checks across all 10 migrations. It records the valid-weight stale-only fleet case, recovery after an hour-future timestamp, current-capacity/reweigh behavior, checkout snapshots and quantity multiplication, rollback/retry, outdoor Spot, mission-plus-live-telemetry dispatch, an HTTP 409 for a not-ready scheduled stop, and multi-stop wrong/correct/replay PIN delivery with telemetry-backed arrivals and stable arrival notification IDs; `physical_flight` remains false. The final Rust suite passes 45 tests (1 intentionally ignored), and cargo check, clippy, formatting and Python compilation pass. The coordinator-owned live HTTP 8081 dispatch/tracking/orders matrix is still in progress with the QA owner against the rebuilt server and migrations through 0010.
