#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
pg_bin="$repo_root/postgres/install/bin"
pg_port="${PGPORT:-55432}"
tmp_dir="$(mktemp -d)"
pg_data="$tmp_dir/data"
pg_log="$tmp_dir/postgres.log"
started=0
cleanup() {
  if (( started )); then "$pg_bin/pg_ctl" -D "$pg_data" -m immediate -w stop >/dev/null 2>&1 || true; fi
  if (( ${1:-0} )); then tail -n 60 "$pg_log" >&2 || true; fi
  rm -rf "$tmp_dir"
}
trap 'cleanup $?' EXIT

"$pg_bin/initdb" -D "$pg_data" -A trust --no-instructions >/dev/null
cat >> "$pg_data/postgresql.conf" <<EOF
port = $pg_port
listen_addresses = ''
unix_socket_directories = '$tmp_dir'
wal_level = logical
max_replication_slots = 4
max_wal_senders = 4
shared_preload_libraries = 'pg_dbsp'
pg_dbsp.database = 'postgres'
EOF
"$pg_bin/pg_ctl" -D "$pg_data" -l "$pg_log" -w start >/dev/null
started=1
psql=("$pg_bin/psql" -X -v ON_ERROR_STOP=1 -h "$tmp_dir" -p "$pg_port" -d postgres)

sql() { "${psql[@]}" -Atq -c "$1"; }
assert_counts() {
  local equal
  equal="$(sql "SELECT (SELECT jsonb_object_agg(group_key, row_count) FROM dbsp.counts)
    IS NOT DISTINCT FROM
    (SELECT jsonb_object_agg(group_key, n) FROM
      (SELECT group_key, count(*) AS n FROM public.ivm_source GROUP BY group_key) s)")"
  [[ "$equal" == t ]]
}
wait_counts() {
  local attempt
  for attempt in {1..100}; do
    if [[ "$(sql "SELECT dbsp.status()->>'state'")" == ready ]] && assert_counts; then return; fi
    sleep 0.2
  done
  sql 'SELECT dbsp.status()' >&2
  sql 'SELECT * FROM dbsp.counts ORDER BY 1 LIMIT 10' >&2
  sql 'SELECT group_key, count(*) FROM public.ivm_source GROUP BY 1 ORDER BY 1 LIMIT 10' >&2
  echo 'count view did not converge' >&2
  return 1
}

sql 'CREATE EXTENSION pg_dbsp' >/dev/null
# Put the primary key second to exercise pgoutput key-tuple column mapping.
sql 'CREATE TABLE public.ivm_source (group_key text NOT NULL, id bigint PRIMARY KEY)' >/dev/null
sql "INSERT INTO public.ivm_source VALUES ('a',1),('a',2),('b',3)" >/dev/null
sql "SELECT dbsp.register_count_view('public.ivm_source'::regclass)" >/dev/null
wait_counts
echo 'initial load: pass'

sql "BEGIN; INSERT INTO public.ivm_source VALUES ('a',4),('c',5); UPDATE public.ivm_source SET group_key='b' WHERE id=2; DELETE FROM public.ivm_source WHERE id=3; COMMIT" >/dev/null
wait_counts
echo 'multirow insert/update/delete: pass'

sql "BEGIN; INSERT INTO public.ivm_source VALUES ('rolled_back',6); ROLLBACK" >/dev/null
wait_counts
[[ "$(sql "SELECT count(*) FROM dbsp.counts WHERE group_key='rolled_back'")" == 0 ]]
echo 'rollback: pass'

"${psql[@]}" -q -c "INSERT INTO public.ivm_source SELECT 'left', generate_series(100,119)" >/dev/null &
left_pid=$!
"${psql[@]}" -q -c "INSERT INTO public.ivm_source SELECT 'right', generate_series(200,219)" >/dev/null &
right_pid=$!
wait "$left_pid"
wait "$right_pid"
wait_counts
echo 'concurrent commits: pass'

sql "UPDATE public.ivm_source SET id=7, group_key='c' WHERE id=5" >/dev/null
wait_counts
echo 'primary-key update: pass'

sql 'TRUNCATE public.ivm_source' >/dev/null
sql "INSERT INTO public.ivm_source VALUES ('after_truncate',8)" >/dev/null
wait_counts
echo 'truncate rebuild: pass'

"$pg_bin/pg_ctl" -D "$pg_data" -m immediate -w stop >/dev/null
started=0
"$pg_bin/pg_ctl" -D "$pg_data" -l "$pg_log" -w start >/dev/null
started=1
wait_counts
sql "INSERT INTO public.ivm_source VALUES ('after_restart',9)" >/dev/null
wait_counts
echo 'crash restart and continued capture: pass'

# Small indicative comparison in the same cluster. The control table has the
# same structure but is not in the publication.
sql "INSERT INTO public.ivm_source SELECT 'bench_'||(n%100), 1000+n FROM generate_series(1,20000) n" >/dev/null
wait_counts
sql 'CREATE TABLE public.ivm_control (LIKE public.ivm_source INCLUDING ALL)' >/dev/null
sql 'INSERT INTO public.ivm_control SELECT * FROM public.ivm_source' >/dev/null
sql 'CREATE MATERIALIZED VIEW public.ivm_refresh AS SELECT group_key, count(*) AS row_count FROM public.ivm_source GROUP BY group_key' >/dev/null
sql 'CREATE SEQUENCE public.ivm_bench_id START 50000' >/dev/null
echo "INSERT INTO public.ivm_control (group_key,id) VALUES ('bench_tx', nextval('public.ivm_bench_id'));" > "$tmp_dir/control.sql"
echo "INSERT INTO public.ivm_source (group_key,id) VALUES ('bench_tx', nextval('public.ivm_bench_id'));" > "$tmp_dir/source.sql"
echo 'REFRESH MATERIALIZED VIEW public.ivm_refresh;' > "$tmp_dir/refresh.sql"
bench_latency() {
  "$pg_bin/pgbench" -n -c 1 -j 1 -t "$2" -f "$1" -h "$tmp_dir" -p "$pg_port" postgres \
    | awk '/latency average/ {print $4}'
}
control_ms="$(bench_latency "$tmp_dir/control.sql" 100)"
source_ms="$(bench_latency "$tmp_dir/source.sql" 100)"
lag_start_ms="$(date +%s%3N)"
wait_counts
lag_ms=$(( $(date +%s%3N) - lag_start_ms ))
refresh_ms="$(bench_latency "$tmp_dir/refresh.sql" 20)"
echo "indicative benchmark (20k base rows): control write avg ${control_ms} ms; source write avg ${source_ms} ms; view catch-up after writes ${lag_ms} ms; full REFRESH avg ${refresh_ms} ms"

# Hold the worker between the durable apply and slot advancement, terminate it,
# and confirm a restart rebuilds the view without double-applying the batch.
sql "ALTER SYSTEM SET pg_dbsp.test_pause_after_apply_ms = '5000'" >/dev/null
"$pg_bin/pg_ctl" -D "$pg_data" -m fast -w stop >/dev/null
started=0
"$pg_bin/pg_ctl" -D "$pg_data" -l "$pg_log" -w start >/dev/null
started=1
wait_counts
old_lsn="$(sql 'SELECT applied_lsn FROM dbsp.config')"
sql "INSERT INTO public.ivm_source VALUES ('replay_gap', 70000)" >/dev/null
gap_seen=0
for attempt in {1..100}; do
  if [[ "$(sql "SELECT applied_lsn > '$old_lsn'::pg_lsn AND applied_lsn >
    (SELECT confirmed_flush_lsn FROM pg_replication_slots WHERE slot_name='pg_dbsp_count_slot')
    FROM dbsp.config")" == t ]]; then gap_seen=1; break; fi
  sleep 0.05
done
[[ "$gap_seen" == 1 ]]
sql "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE backend_type='pg_dbsp count worker'" >/dev/null
"$pg_bin/pg_ctl" -D "$pg_data" -m immediate -w stop >/dev/null
started=0
"$pg_bin/pg_ctl" -D "$pg_data" -l "$pg_log" -w start >/dev/null
started=1
wait_counts
[[ "$(sql "SELECT row_count FROM dbsp.counts WHERE group_key='replay_gap'")" == 1 ]]
echo 'replay gap before slot advancement: pass'

sql 'DROP MATERIALIZED VIEW public.ivm_refresh' >/dev/null
sql 'ALTER TABLE public.ivm_source ALTER COLUMN group_key TYPE varchar(100)' >/dev/null
for attempt in {1..50}; do
  [[ "$(sql "SELECT dbsp.status()->>'state'")" == error ]] && break
  sleep 0.2
done
[[ "$(sql "SELECT dbsp.status()->>'state'")" == error ]]
echo 'incompatible schema: pass'
echo 'integration tests passed'
