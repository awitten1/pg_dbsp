# Native Feldera + PostgreSQL CDC experiment

This runs two host processes: the repository's PostgreSQL 18 build and a local
Feldera pipeline manager. There are no containers or cloud services. PostgreSQL
publishes `public.events` through logical replication; Feldera snapshots it,
follows changes, and incrementally maintains `group_totals`.

## Start

From the repository root:

```sh
./build_postgres.sh                         # skip if postgres/install already exists
./experiments/feldera_local/run.sh start
./experiments/feldera_local/run.sh status
```

The first start downloads pinned Feldera v0.378.0 release files into the ignored
`.tools/feldera` directory, installs Rust 1.99 with rustup if necessary, and
compiles the pipeline's native Rust executable. This first compile can take
several minutes; the compiler cache used about 30 GB on this machine. You need `curl`, `unzip`,
`tar`, `jq`, `python3`, `java`, `rustup`, and a C/C++ build toolchain. The setup
script checks the release downloads against fixed SHA-256 hashes.

Open the local Feldera console at <http://127.0.0.1:18080>. PostgreSQL listens
on `127.0.0.1:55434`, and `run.sh sql` opens `psql` there. This experiment uses
PostgreSQL trust authentication on loopback and the current OS user as a
PostgreSQL superuser, so keep it on a development machine.

## Try changes

The initial PostgreSQL rows are `(1, east, 10)`, `(2, east, 20)`, and
`(3, west, 7)`. `run.sh status` shows the PostgreSQL `GROUP BY`, Feldera's
materialized view, and the replication slot's acknowledged LSN.

```sh
./experiments/feldera_local/run.sh sql -c "INSERT INTO public.events VALUES (4, 'west', 5)"
./experiments/feldera_local/run.sh status

./experiments/feldera_local/run.sh sql -c "UPDATE public.events SET group_key = 'west', amount = 25 WHERE id = 2"
./experiments/feldera_local/run.sh status

./experiments/feldera_local/run.sh sql -c 'DELETE FROM public.events WHERE id = 1'
./experiments/feldera_local/run.sh status
```

Allow a moment for Feldera to consume each transaction before comparing
outputs. To watch changes directly, run this in a separate terminal:

```sh
curl -N -X POST 'http://127.0.0.1:18080/v0/pipelines/feldera-local-cdc/egress/GROUP_TOTALS?format=json&send_snapshot=true'
```

Edit [program.sql](program.sql) to try a different Feldera query. Its source
URI contains `__PG_USER__`, which `run.sh` replaces with the current OS user.
Keep the `events` input and `group_totals` view names if you want `run.sh
status` and its startup snapshot check to work unchanged. After editing, run
`stop`, `reset`, then `start` to compile the new program against a fresh source.
The source setup is in [postgres.sql](postgres.sql). The manager uses a separate
`feldera_manager` database for its metadata; its CDC connector installs an
`etl` schema in the source `postgres` database to track replication progress.
Neither is a table of DBSP operator state.

## Stop or start fresh

```sh
./experiments/feldera_local/run.sh stop
./experiments/feldera_local/run.sh reset
./experiments/feldera_local/run.sh start
```

`reset` deletes **only this experiment's PostgreSQL data** and runner state in
`.tools/feldera`. It keeps the Feldera download and compilation cache. A fresh
snapshot is required after a stopped open source pipeline because this setup
does not configure Feldera fault tolerant checkpoints.

References: [Feldera's PostgreSQL CDC connector](https://docs.feldera.com/connectors/sources/postgresql-cdc/),
[native source startup](https://github.com/feldera/feldera#%EF%B8%8F-running-feldera-from-sources),
and [REST API tutorial](https://docs.feldera.com/tutorials/rest_api/).
