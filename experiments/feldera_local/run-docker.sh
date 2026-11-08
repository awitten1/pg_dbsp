#!/usr/bin/env bash
# Apple Silicon PostgreSQL -> Feldera CDC demo. No host Rust/Postgres required.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$root/experiments/feldera_local"
state="$root/.tools/feldera-docker"
marker="$state/started.marker"
pipeline_url='http://127.0.0.1:18080/v0/pipelines/feldera-local-cdc'

usage() {
    echo "Usage: $0 {start|status|sql [psql arguments...]|logs [service]|stop|reset}"
}

die() { echo "$*" >&2; exit 1; }

compose() {
    docker compose -f "$here/compose-docker.yml" "$@"
}

psql_demo() {
    compose exec -T postgres psql -X -v ON_ERROR_STOP=1 -U postgres -d postgres "$@"
}

show_status() {
    compose ps
    curl -fsS "$pipeline_url" | jq '{program_status, deployment_status, deployment_error}'
    echo 'PostgreSQL source totals:'
    psql_demo -c 'SELECT group_key, COUNT(*) AS row_count, SUM(amount) AS total_amount FROM public.events GROUP BY group_key ORDER BY group_key'
    echo 'Feldera materialized totals:'
    curl -fsS -G "$pipeline_url/query" \
        --data-urlencode 'sql=SELECT * FROM group_totals ORDER BY group_key' --data-urlencode 'format=text'
    echo
    echo 'PostgreSQL output table:'
    psql_demo -c 'SELECT * FROM public.feldera_group_totals ORDER BY group_key'
    echo "Output matches source aggregate: $(psql_demo -Atq < "$here/compare.sql")"
    echo 'Replication slot:'
    psql_demo -c "SELECT slot_name, confirmed_flush_lsn FROM pg_replication_slots WHERE plugin='pgoutput'"
}

case "${1:-help}" in
    start)
        if [[ -f "$marker" ]]; then
            if [[ -n "$(compose ps --status running -q pipeline-manager)" ]] && \
                [[ "$(curl -fsS "$pipeline_url" | jq -r '.deployment_status')" == Running ]]; then
                echo 'This experiment is already running.'
                show_status
                exit 0
            fi
            die "This experiment has already attempted a snapshot. Run '$0 reset' before starting again."
        fi
        echo 'Starting PostgreSQL...'
        compose up -d --wait --wait-timeout 120 postgres
        manager_db="$(psql_demo -Atq -c "SELECT 1 FROM pg_database WHERE datname='feldera_manager'")"
        if [[ "$manager_db" != 1 ]]; then
            compose exec -T postgres createdb -U postgres feldera_manager
        fi
        psql_demo < "$here/postgres.sql"
        echo 'Starting Feldera...'
        compose up -d --wait --wait-timeout 360 pipeline-manager

        # Upload the SQL file as written.
        jq -n --rawfile code "$here/program-docker.sql" '{
            name: "feldera-local-cdc",
            runtime_config: {workers: 2, provisioning_timeout: "3m"},
            program_config: {profile: "optimized"},
            program_code: $code
        }' |
            curl -fsS -X PUT "$pipeline_url" -H 'Content-Type: application/json' -d @- >/dev/null

        # Wait for compilation.
        for ((attempt=0; attempt<180; attempt++)); do
            result="$(curl -fsS "$pipeline_url")"
            compilation="$(jq -r '.program_status' <<< "$result")"
            case "$compilation" in
                Success) break ;;
                SqlError|RustError|SystemError)
                    jq '.program_error' <<< "$result" >&2
                    die 'Pipeline compilation failed.' ;;
            esac
            echo "Feldera compilation: $compilation"
            sleep 10
        done
        [[ "$compilation" == Success ]] || die 'Pipeline compilation timed out.'

        # Record the snapshot attempt before starting, so failures require reset.
        mkdir -p "$state"
        touch "$marker"
        curl -fsS -X POST "$pipeline_url/start" >/dev/null

        for ((attempt=0; attempt<180; attempt++)); do
            result="$(curl -fsS "$pipeline_url")"
            deployment="$(jq -r '.deployment_status' <<< "$result")"
            [[ "$deployment" == Running ]] && break
            if [[ "$(jq -r '.deployment_error' <<< "$result")" != null ]]; then
                jq '.deployment_error' <<< "$result" >&2
                die 'Pipeline deployment failed; reset before retrying.'
            fi
            echo "Feldera deployment: $deployment"
            sleep 2
        done
        [[ "$deployment" == Running ]] || die 'Pipeline deployment timed out.'

        # Wait for the initial snapshot to reach the output table.
        snapshot_ready=false
        for ((attempt=0; attempt<180; attempt++)); do
            result="$(curl -fsS "$pipeline_url")"
            [[ "$(jq -r '.deployment_status' <<< "$result")" == Running ]] || die 'Pipeline stopped during its snapshot.'
            if view="$(curl -fsS -G "$pipeline_url/query" \
                --data-urlencode 'sql=SELECT * FROM group_totals ORDER BY group_key' \
                --data-urlencode 'format=text')" && \
                [[ "$view" == *east* && "$view" == *west* ]] && \
                [[ "$(psql_demo -Atq < "$here/compare.sql")" == t ]]; then
                snapshot_ready=true
                break
            fi
            echo 'Waiting for CDC snapshot and PostgreSQL output...'
            sleep 2
        done
        [[ "$snapshot_ready" == true ]] || die 'Snapshot did not converge; inspect status and logs.'
        echo 'Initial snapshot and PostgreSQL output match the source aggregate.'
        echo 'Console: http://127.0.0.1:18080'
        echo 'PostgreSQL: postgresql://postgres:feldera-local@127.0.0.1:55434/postgres'
        echo "SQL shell: $0 sql"
        ;;
    status) show_status ;;
    sql)
        shift
        # Allocate a terminal only for an interactive shell, keeping SQL input
        # pipes and automation usable without a TTY.
        if [[ -t 0 && -t 1 ]]; then
            compose exec postgres psql -X -v ON_ERROR_STOP=1 -U postgres -d postgres "$@"
        else
            psql_demo "$@"
        fi
        ;;
    logs)
        shift
        compose logs --follow --tail 100 "$@"
        ;;
    stop)
        compose stop
        echo 'Stopped the Docker experiment. Reset before starting a fresh snapshot.'
        ;;
    reset)
        compose down
        # These explicit names belong only to this experiment. Never use a
        # global prune or down --volumes, which would discard the compiler cache.
        for volume in pg_dbsp_feldera_docker_pgdata pg_dbsp_feldera_docker_runner_state; do
            if docker volume inspect "$volume" >/dev/null 2>&1; then docker volume rm "$volume"; fi
        done
        rm -f "$marker"
        echo 'Removed Docker experiment data and runner state; compiler cache retained.'
        ;;
    help|-h|--help) usage ;;
    *) usage >&2; exit 1 ;;
esac
