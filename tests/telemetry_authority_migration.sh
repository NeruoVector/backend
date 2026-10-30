#!/usr/bin/env bash
# Run only against a disposable PostgreSQL container and empty qa_telemetry_* database.
set -euo pipefail
: "${QA_DISPOSABLE:?set QA_DISPOSABLE=yes}"
[ "$QA_DISPOSABLE" = yes ] || { printf '%s\n' 'QA_DISPOSABLE must be yes' >&2; exit 1; }
: "${QA_PG_CONTAINER:?set QA_PG_CONTAINER}"
: "${QA_PG_DATABASE:?set QA_PG_DATABASE}"
case "$QA_PG_CONTAINER" in qa-telemetry-*) ;; *) printf '%s\n' 'container must start with qa-telemetry-' >&2; exit 1;; esac
case "$QA_PG_DATABASE" in qa_telemetry_*) ;; *) printf '%s\n' 'database name must start with qa_telemetry_' >&2; exit 1;; esac
root=$(cd "$(dirname "$0")/.." && pwd)
psqlqa() { docker exec -i "$QA_PG_CONTAINER" psql -U postgres -d "$QA_PG_DATABASE" -v ON_ERROR_STOP=1 -At; }
printf '%s\n' 'CREATE SCHEMA neurovector; CREATE EXTENSION IF NOT EXISTS "uuid-ossp";' | psqlqa > /dev/null
for migration in "$root"/migrations/000*.sql "$root"/migrations/0010*.sql "$root"/migrations/0011*.sql; do
    psqlqa < "$migration" > /dev/null
done
psqlqa > /dev/null <<'SQL'
INSERT INTO neurovector.telemetry(drone_id,ts,received_at,status,raw) VALUES
 ('legacy',now(),now(),'flying','{}'),
 ('legacy','2000-01-01',now(),'landed','{}'),
 ('legacy',now()+interval '1 hour',now(),'failed','{}');
SQL
psqlqa < "$root/migrations/0013_telemetry_authority.sql" > /dev/null
psqlqa <<'SQL'
DO $$ BEGIN
 IF (SELECT count(*) FROM neurovector.telemetry WHERE drone_id='legacy') <> 3
    OR (SELECT count(*) FROM neurovector.telemetry WHERE drone_id='legacy' AND accepted_for_current_state) <> 0
    OR EXISTS (SELECT 1 FROM neurovector.current_drone_telemetry WHERE drone_id='legacy') THEN
   RAISE EXCEPTION 'legacy rows must be preserved without authority';
 END IF;
END $$;
INSERT INTO neurovector.telemetry(drone_id,ts,received_at,status,raw,accepted_for_current_state) VALUES
 ('legacy',now(),now(),'flying','{}',true),
 ('legacy',now()+interval '1 hour',now(),'landed','{}',false);
DO $$ BEGIN
 IF (SELECT status FROM neurovector.current_drone_telemetry WHERE drone_id='legacy') <> 'flying'
    OR (SELECT count(*) FROM neurovector.current_drone_telemetry WHERE drone_id='legacy') <> 1 THEN
   RAISE EXCEPTION 'ignored rows must not replace the accepted current row';
 END IF;
END $$;
SQL
snapshot() {
    printf '%s\n' "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY id),'[]'::jsonb) FROM neurovector.telemetry t; SELECT coalesce(jsonb_agg(to_jsonb(v) ORDER BY id),'[]'::jsonb) FROM neurovector.current_drone_telemetry v;" | psqlqa
}
before=$(snapshot)
psqlqa < "$root/migrations/0013_telemetry_authority.sql" > /dev/null
after=$(snapshot)
if [ "$before" != "$after" ]; then
    printf '%s\n' 'FAIL migration replay changed telemetry or current-view snapshot' >&2
    exit 1
fi
printf '%s\n' 'PASS legacy history, current view and complete snapshot survive migration replay'
