# pg_dbsp

An initial [pgrx](https://github.com/pgcentralfoundation/pgrx) extension scaffold for PostgreSQL 18, with [DBSP](https://github.com/feldera/feldera) linked in. It does not implement incremental view maintenance yet. The SQL smoke function builds and runs a tiny DBSP circuit so both dependencies are exercised.

## How the scaffold was generated

The base extension was generated with `cargo-pgrx` 0.17.0 in a temporary directory, then its files were copied into this existing repository root:

```sh
cargo install cargo-pgrx --version 0.17.0 --locked
cd "$(mktemp -d)"
cargo pgrx new pg_dbsp
# Copy the generated files from pg_dbsp/ into this repository root.
```

The generated pgrx layout, control-file placeholders, example SQL function, test harness, and platform configuration are retained. Project-specific changes set version 0.1.0 and PostgreSQL 18 as the default, pin Rust and DBSP versions, add `dbsp_smoke_test()`, disable unused doctests, and keep `Cargo.lock` for reproducible builds. Local PostgreSQL data and build tools are ignored by Git.

## Build and try it

The repository's `build_postgres.sh` installs PostgreSQL 18 under `postgres/install`. Rust 1.91.0 is selected by `rust-toolchain.toml`. Install that Rust toolchain and PostgreSQL first if needed. The Cargo configuration selects C11 for the DBSP version's bundled mimalloc code on GCC 15.

```sh
cargo install cargo-pgrx --version 0.17.0 --locked
export PGRX_HOME="$PWD/.pgrx"
cargo pgrx init --pg18 "$PWD/postgres/install/bin/pg_config"
cargo pgrx install --pg-config "$PWD/postgres/install/bin/pg_config"
cargo pgrx test pg18
postgres/install/bin/pg_ctl -D .pgrx/data-18 -l .pgrx/postgres.log \
  -o '-p 5433 -c listen_addresses=localhost' -w start
postgres/install/bin/psql -h localhost -p 5433 -U "$(id -un)" -d postgres \
  -v ON_ERROR_STOP=1 \
  -c 'CREATE EXTENSION IF NOT EXISTS pg_dbsp;' \
  -c 'SELECT dbsp_smoke_test();'
postgres/install/bin/pg_ctl -D .pgrx/data-18 -w stop
```

`dbsp_smoke_test()` should return `t`. Set `LIBCLANG_PATH` to your LLVM library directory if bindgen cannot find libclang. The `sql/` directory is reserved for future handwritten extension upgrade scripts; pgrx generates the initial install SQL during `cargo pgrx install`.

## Native Feldera comparison

The [local Feldera CDC experiment](experiments/feldera_local/README.md) runs
Feldera and a separate PostgreSQL 18 cluster as host processes. It snapshots a
source table through PostgreSQL logical replication and maintains an aggregate
view for a native CDC trial.
