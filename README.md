# pg_dbsp

A PostgreSQL 18 research extension that maintains one asynchronous count view with [DBSP](https://github.com/feldera/feldera). Register an ordinary table with `id bigint PRIMARY KEY` and `group_key text NOT NULL` using a deterministic collation; a pgrx background worker maintains counts in `dbsp.counts`. Reads are eventually consistent. This prototype supports one source table in one configured database.

PostgreSQL reserves schema names beginning with `pg_`, so the SQL API uses `dbsp` even though the extension and library are named `pg_dbsp`.

## How the scaffold was generated

The base extension was generated with `cargo-pgrx` 0.17.0 in a temporary directory and copied into this existing repository root:

```sh
cargo install cargo-pgrx --version 0.17.0 --locked
cd "$(mktemp -d)"
cargo pgrx new pg_dbsp
# Copy the generated pg_dbsp/ files into the repository root.
```

The pgrx layout, control file placeholders, SQL generation, test harness, and platform configuration came from that command. The project then pinned PostgreSQL 18, Rust, pgrx, and DBSP; added the worker, logical decoding, DBSP count circuit, SQL API, and tests. The `Cargo.lock` is committed for reproducibility. The repository's `.cargo/config.toml` selects C11 for DBSP's bundled mimalloc on GCC 15.

## Build and start

The `build_postgres.sh` script builds PostgreSQL 18 under `postgres/install`. Install the Rust version in `rust-toolchain.toml`, `cargo-pgrx` 0.17.0, and libclang. Set `LIBCLANG_PATH` if bindgen cannot find libclang.

For a fresh local demo, run `./start_pg_dbsp_demo.sh`. It installs the extension, initializes `.pgrx/demo-data` on first use, starts the worker with logical WAL enabled, creates the extension in `postgres`, and prints the `psql` connection command. Run `./start_pg_dbsp_demo.sh stop` to stop it. Set `PG_DBSP_PORT` to change the default port (55433).

```sh
export PGRX_HOME="$PWD/.pgrx"
cargo pgrx init --pg18 "$PWD/postgres/install/bin/pg_config"
cargo pgrx install --pg-config "$PWD/postgres/install/bin/pg_config"

postgres/install/bin/pg_ctl -D .pgrx/data-18 -l .pgrx/postgres.log \
  -o '-p 5433 -c listen_addresses=localhost -c wal_level=logical -c shared_preload_libraries=pg_dbsp -c pg_dbsp.database=postgres' -w start
postgres/install/bin/psql -h localhost -p 5433 -d postgres \
  -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_dbsp;'
```

`pg_dbsp.database` must name the single database the worker connects to. Set `wal_level=logical` and preload the library before registering a source. The worker creates publication `pg_dbsp_count_pub` and logical slot `pg_dbsp_count_slot`. PostgreSQL must have free replication slot capacity. Use a UTF-8 database; the first decoder handles pgoutput protocol version 1 text tuples.

```sql
CREATE TABLE public.events (id bigint PRIMARY KEY, group_key text NOT NULL);
INSERT INTO public.events VALUES (1, 'east'), (2, 'east'), (3, 'west');
SELECT dbsp.register_count_view('public.events'::regclass);

SELECT * FROM dbsp.counts ORDER BY group_key;
SELECT dbsp.status();
```

`dbsp.status()` returns the source, worker state, worker liveness, durable applied LSN, and last error. Wait for `state = ready` before reading initial counts. The worker polls roughly once per second, so a committed write may appear later. Source transactions are applied as units. `TRUNCATE` triggers a locked rebuild. A worker restart also rebuilds from a locked snapshot. Unsupported source schema changes put the worker in `error` state.

## How maintenance works

The worker creates a `pgoutput` slot and publication, locks the source against writes, scans `(id, group_key)`, loads an empty DBSP circuit, replaces `dbsp.counts`, and records a WAL boundary in one transaction. It peeks committed changes without consuming them. For each source transaction, it uses the primary key to retract old rows for updates and deletes, runs the DBSP count circuit, and writes changed groups plus the applied LSN in one PostgreSQL transaction. Only after that commit does it advance the slot. The durable LSN suppresses replay if the slot lags. On restart or apply failure, the worker rebuilds the circuit and view from a new locked snapshot.

This is a focused prototype: it does not compile arbitrary SQL, maintain joins, checkpoint DBSP state, or optimize restart time. The worker and slot have fixed names; use one registered source per configured database. To change source in this version, drop and recreate the extension and its capture objects in a test database.

## Tests

```sh
export PGRX_HOME="$PWD/.pgrx"
cargo pgrx test pg18
tests/ivm_integration.sh
```

The pgrx tests check the circuit and SQL smoke function. The integration script creates a temporary PostgreSQL 18 cluster with the worker preloaded, then checks initial load, inserts, deletes, updates, multirow transactions, rollback, concurrent commits, `TRUNCATE`, crash restart, and an interrupted slot advance against a fresh SQL `GROUP BY`. It also checks incompatible schema changes. Run `cargo pgrx install` first. Set `PGPORT` to choose a free test port. The script temporarily sets `pg_dbsp.test_pause_after_apply_ms` to open a repeatable fault window; its default is zero.

The script uses `pgbench` to compare 100 single-row committed writes to the captured table and an otherwise identical uncaptured table, then measures time to catch up and 20 full materialized-view refreshes. One local debug-build run with 20,000 base rows measured 0.013 ms mean uncaptured write latency, 0.011 ms mean captured write latency, about 1.9 s to observe the asynchronous view catch up after the burst, and 1.68 ms mean full refresh latency. These are small local measurements rather than a performance claim; run the script on the intended hardware and workload for useful sizing.

## Native Feldera comparison

The [local Feldera CDC experiment](experiments/feldera_local/README.md) runs
Feldera and a separate PostgreSQL 18 cluster as host processes. It snapshots a
source table through PostgreSQL logical replication and maintains an aggregate
view for a native CDC trial.
