# pg_dbsp

An initial [pgrx](https://github.com/pgcentralfoundation/pgrx) extension scaffold for PostgreSQL 18, with [DBSP](https://github.com/feldera/feldera) linked in. It does not implement incremental view maintenance yet. The SQL smoke function builds and runs a tiny DBSP circuit so both dependencies are exercised.

## Build and try it

The repository's `build_postgres.sh` installs PostgreSQL 18 under `postgres/install`. Rust 1.91.0 is selected by `rust-toolchain.toml`. Install that Rust toolchain and PostgreSQL first if needed. The Cargo configuration selects C11 for the DBSP version's bundled mimalloc code on GCC 15.

```sh
cargo install cargo-pgrx --version 0.17.0 --locked
export PGRX_HOME="$PWD/.pgrx"
cargo pgrx init --pg18 "$PWD/postgres/install/bin/pg_config"
cargo pgrx install --pg-config "$PWD/postgres/install/bin/pg_config"
postgres/install/bin/pg_ctl -D .pgrx/data-18 -l .pgrx/postgres.log \
  -o '-p 5433 -c listen_addresses=localhost' -w start
postgres/install/bin/psql -h localhost -p 5433 -U "$(id -un)" -d postgres \
  -v ON_ERROR_STOP=1 \
  -c 'CREATE EXTENSION IF NOT EXISTS pg_dbsp;' \
  -c 'SELECT dbsp_smoke_test();'
postgres/install/bin/pg_ctl -D .pgrx/data-18 -w stop
```

`dbsp_smoke_test()` should return `t`. Set `LIBCLANG_PATH` to your LLVM library directory if bindgen cannot find libclang. The `sql/` directory is reserved for future handwritten extension upgrade scripts; pgrx generates the initial install SQL during `cargo pgrx install`.
