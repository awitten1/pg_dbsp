#!/usr/bin/env bash
# Start a local PostgreSQL 18 cluster with the pg_dbsp worker preloaded.
# Usage: ./start_pg_dbsp_demo.sh [start|stop]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pg_bin="$root/postgres/install/bin"
export PGRX_HOME="${PGRX_HOME:-$root/.pgrx}"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.91.0}"
data_dir="${PG_DBSP_DATA_DIR:-$PGRX_HOME/demo-data}"
port="${PG_DBSP_PORT:-55433}"
log_file="$data_dir/server.log"

if [[ -z "${LIBCLANG_PATH:-}" && -d /usr/lib/llvm-17/lib ]]; then
  export LIBCLANG_PATH=/usr/lib/llvm-17/lib
fi

for binary in pg_config initdb pg_ctl psql; do
  if [[ ! -x "$pg_bin/$binary" ]]; then
    echo "Missing $pg_bin/$binary; build PostgreSQL first with ./build_postgres.sh" >&2
    exit 1
  fi
done

if [[ ! "$port" =~ ^[0-9]+$ ]] || (( port < 1 || port > 65535 )); then
  echo "PG_DBSP_PORT must be a TCP port from 1 to 65535" >&2
  exit 1
fi

case "${1:-start}" in
  start)
    if [[ -f "$data_dir/PG_VERSION" ]] && "$pg_bin/pg_ctl" -D "$data_dir" status >/dev/null 2>&1; then
      echo "PostgreSQL is already running from $data_dir"
    else
      if [[ -x "$root/.tools/bin/cargo-pgrx" ]]; then
        pgrx=("$root/.tools/bin/cargo-pgrx" pgrx)
      else
        pgrx=(cargo pgrx)
      fi
      mkdir -p "$PGRX_HOME"
      if [[ ! -f "$PGRX_HOME/config.toml" ]]; then
        "${pgrx[@]}" init --pg18 "$pg_bin/pg_config"
      fi
      "${pgrx[@]}" install --pg-config "$pg_bin/pg_config"

      if [[ ! -f "$data_dir/PG_VERSION" ]]; then
        if [[ -e "$data_dir" ]]; then
          echo "Data directory exists but is not initialized: $data_dir" >&2
          exit 1
        fi
        mkdir -p "$(dirname "$data_dir")"
        "$pg_bin/initdb" -D "$data_dir" -A trust --no-instructions
      fi
      "$pg_bin/pg_ctl" -D "$data_dir" -l "$log_file" \
        -o "-p $port -c listen_addresses=localhost -c wal_level=logical -c shared_preload_libraries=pg_dbsp -c pg_dbsp.database=postgres" \
        -w start
    fi

    "$pg_bin/psql" -X -v ON_ERROR_STOP=1 -h localhost -p "$port" -d postgres \
      -c 'CREATE EXTENSION IF NOT EXISTS pg_dbsp;'
    if [[ "$("$pg_bin/psql" -X -Atq -h localhost -p "$port" -d postgres \
      -c "SELECT to_regprocedure('dbsp.status()') IS NOT NULL")" != t ]]; then
      echo "The installed extension lacks dbsp.status(); use a fresh demo cluster" >&2
      exit 1
    fi
    printf 'Connect with:\n  %q -h localhost -p %q -d postgres\n' "$pg_bin/psql" "$port"
    ;;
  stop)
    if [[ -f "$data_dir/PG_VERSION" ]] && "$pg_bin/pg_ctl" -D "$data_dir" status >/dev/null 2>&1; then
      "$pg_bin/pg_ctl" -D "$data_dir" -w stop
    else
      echo "PostgreSQL is not running from $data_dir"
    fi
    ;;
  *)
    echo "Usage: $0 [start|stop]" >&2
    exit 1
    ;;
esac
