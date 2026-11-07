#!/usr/bin/env bash
# Native PostgreSQL -> Feldera logical replication experiment.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$root/experiments/feldera_local"
state="$root/.tools/feldera"
pg_bin="$root/postgres/install/bin"
pg_data="$state/pgdata"
pg_user="$(id -un)"
pg_port=55434
api='http://127.0.0.1:18080/v0'
pipeline='feldera-local-cdc'

psql_demo() {
    "$pg_bin/psql" -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$pg_port" -U "$pg_user" -d postgres "$@"
}

start_postgres() {
    [[ -x "$pg_bin/pg_ctl" ]] || {
        echo "Build PostgreSQL first with $root/build_postgres.sh" >&2
        exit 1
    }
    if [[ ! -f "$pg_data/PG_VERSION" ]]; then
        "$pg_bin/initdb" -D "$pg_data" -A trust -U "$pg_user" --no-instructions
    fi
    if ! "$pg_bin/pg_ctl" -D "$pg_data" status >/dev/null 2>&1; then
        "$pg_bin/pg_ctl" -D "$pg_data" -l "$state/postgres.log" \
            -o "-p $pg_port -c listen_addresses=127.0.0.1 -c wal_level=logical" -w start
    fi
    if [[ "$(psql_demo -Atc 'SHOW wal_level')" != logical ]]; then
        echo 'The experiment PostgreSQL server must have wal_level=logical.' >&2
        exit 1
    fi
    if [[ "$(psql_demo -Atc "SELECT 1 FROM pg_database WHERE datname = 'feldera_manager'")" != 1 ]]; then
        "$pg_bin/createdb" -h 127.0.0.1 -p "$pg_port" -U "$pg_user" feldera_manager
    fi
    psql_demo -f "$here/postgres.sql"
}

manager_running() {
    curl -fsS --max-time 2 "$api/pipelines" >/dev/null 2>&1
}

start_manager() {
    if manager_running; then
        return
    fi
    mkdir -p "$state/compiler" "$state/runner" "$state/demos"
    cd "$root"
    nohup "$state/pipeline-manager" \
        --api-port 18080 --compiler-port 18085 --runner-port 18089 \
        --db-connection-string "postgres://$pg_user@127.0.0.1:$pg_port/feldera_manager" \
        --sql-compiler-path "$state/sql2dbsp-jar-with-dependencies-v0.378.0.jar" \
        --dbsp-override-path "$state/source" \
        --compilation-cargo-lock-path "$state/source/Cargo.lock" \
        --compiler-working-directory "$state/compiler" \
        --runner-working-directory "$state/runner" \
        --compilation-profile dev --demos-dir "$state/demos" \
        >"$state/manager.log" 2>&1 &
    echo $! > "$state/manager.pid"
    for ((i=0; i<60; i++)); do
        if manager_running; then return; fi
        if ! kill -0 "$(cat "$state/manager.pid")" 2>/dev/null; then
            tail -40 "$state/manager.log" >&2
            exit 1
        fi
        sleep 1
    done
    tail -40 "$state/manager.log" >&2
    echo 'Feldera did not become ready.' >&2
    exit 1
}

pipeline_status() {
    curl -fsS "$api/pipelines/$pipeline"
}

deploy_pipeline() {
    sed "s/__PG_USER__/$pg_user/g" "$here/program.sql" > "$state/program.sql"
    jq -n --rawfile code "$state/program.sql" \
        '{name:"feldera-local-cdc",description:"Local PostgreSQL CDC experiment",runtime_config:{workers:2},program_config:{profile:"dev"},program_code:$code}' |
        curl -fsS -X PUT "$api/pipelines/$pipeline" \
            -H 'Content-Type: application/json' --data-binary @- >/dev/null
    for ((i=0; i<120; i++)); do
        local status
        status="$(pipeline_status | jq -r .program_status)"
        case "$status" in
            Success) echo 'Feldera SQL and Rust compilation succeeded.'; return ;;
            SqlError|RustError|SystemError)
                pipeline_status | jq .program_error >&2
                exit 1 ;;
        esac
        if (( i % 6 == 0 )); then echo "Feldera compilation: $status"; fi
        sleep 10
    done
    echo 'Feldera compilation timed out; inspect .tools/feldera/compiler.' >&2
    exit 1
}

start_pipeline() {
    curl -fsS -X POST "$api/pipelines/$pipeline/start" >/dev/null
    for ((i=0; i<120; i++)); do
        local status error
        status="$(pipeline_status | jq -r .deployment_status)"
        case "$status" in
            Running) touch "$state/started.marker"; return ;;
            Stopped)
                error="$(pipeline_status | jq -r '.deployment_error.message // empty')"
                [[ -z "$error" ]] && { sleep 1; continue; }
                pipeline_status | jq .deployment_error >&2
                exit 1 ;;
            Failed)
                pipeline_status | jq .deployment_error >&2
                exit 1 ;;
        esac
        sleep 1
    done
    echo 'Feldera pipeline startup timed out.' >&2
    exit 1
}

wait_for_snapshot() {
    local view
    for ((i=0; i<180; i++)); do
        view="$(curl -fsSG "$api/pipelines/$pipeline/query" \
            --data-urlencode 'sql=SELECT * FROM group_totals ORDER BY group_key' \
            --data-urlencode 'format=text' 2>/dev/null || true)"
        if [[ "$view" == *east* && "$view" == *west* ]]; then
            echo 'Feldera initial PostgreSQL snapshot is visible.'
            return
        fi
        if (( i % 10 == 0 )); then echo 'Waiting for PostgreSQL CDC snapshot...'; fi
        sleep 1
    done
    pipeline_status | jq '{deployment_status,deployment_error,connectors}' >&2
    echo 'Feldera did not show the initial rows within 3 minutes.' >&2
    exit 1
}

case "${1:-}" in
    start)
        if [[ -f "$state/started.marker" ]]; then
            echo 'This experiment has already run. Use status, or stop then reset for a fresh snapshot.' >&2
            exit 1
        fi
        "$here/install.sh"
        start_postgres
        start_manager
        deploy_pipeline
        start_pipeline
        wait_for_snapshot
        echo "Feldera is running at http://127.0.0.1:18080"
        echo "Try: $here/run.sh status"
        ;;
    status)
        if ! manager_running; then echo 'Feldera is not running.'; exit 1; fi
        pipeline_status | jq '{program_status,deployment_status,deployment_error}'
        echo 'PostgreSQL source totals:'
        psql_demo -c 'SELECT group_key, COUNT(*) AS row_count, SUM(amount) AS total_amount FROM public.events GROUP BY group_key ORDER BY group_key'
        echo 'Feldera materialized totals:'
        curl -fsSG "$api/pipelines/$pipeline/query" \
            --data-urlencode 'sql=SELECT * FROM group_totals ORDER BY group_key' \
            --data-urlencode 'format=text'
        echo
        echo 'Replication slot:'
        psql_demo -c "SELECT slot_name, confirmed_flush_lsn FROM pg_replication_slots WHERE plugin = 'pgoutput'"
        ;;
    sql)
        shift
        psql_demo "$@"
        ;;
    stop)
        if manager_running; then
            curl -fsS -X POST "$api/pipelines/$pipeline/shutdown" >/dev/null || true
            for ((i=0; i<30; i++)); do
                [[ "$(pipeline_status | jq -r .deployment_status)" == Stopped ]] && break
                sleep 1
            done
        fi
        if [[ -f "$state/manager.pid" ]]; then
            manager_pid="$(cat "$state/manager.pid")"
            if [[ -r "/proc/$manager_pid/cmdline" ]] &&
                tr '\0' ' ' < "/proc/$manager_pid/cmdline" | grep -Fq "$state/pipeline-manager"; then
                kill "$manager_pid"
            fi
        fi
        if [[ -f "$pg_data/PG_VERSION" ]] && "$pg_bin/pg_ctl" -D "$pg_data" status >/dev/null 2>&1; then
            "$pg_bin/pg_ctl" -D "$pg_data" -w stop
        fi
        echo 'Stopped local Feldera and PostgreSQL.'
        ;;
    reset)
        "$0" stop
        rm -rf "$pg_data" "$state/runner" "$state/started.marker"
        echo 'Removed experiment data. The Feldera install and compiler cache remain.'
        ;;
    *)
        echo "Usage: $0 {start|status|sql|stop|reset}" >&2
        exit 1
        ;;
esac
