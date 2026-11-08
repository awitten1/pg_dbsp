# Feldera + PostgreSQL CDC experiment

## macOS / Apple Silicon (Docker)

Start Docker Desktop first, then use the script below with `jq` and `curl`.
Host Rust, Java, and PostgreSQL builds are not needed. The pinned Feldera
**0.378.0** image and PostgreSQL **18.6** run as Linux ARM64 containers.

From the repository root:

```sh
./experiments/feldera_local/run-docker.sh start
./experiments/feldera_local/run-docker.sh status
./experiments/feldera_local/run-docker.sh sql -c "INSERT INTO public.events VALUES (4, 'west', 5)"
./experiments/feldera_local/run-docker.sh sql
./experiments/feldera_local/run-docker.sh logs
```

The script waits for both services, deploys `program-docker.sql`, and verifies
that the initial snapshot reaches the PostgreSQL output table. Pipeline compilation may
take several minutes; progress and startup errors are printed. Use `logs` to
inspect service logs. The first compiler volume is seeded with the image's
precompiled dependencies. The pipeline uses the matching
`optimized` compilation profile, two Cargo compilation jobs, and two workers.

Open <http://127.0.0.1:18080>. PostgreSQL is available at
`postgresql://postgres:feldera-local@127.0.0.1:55434/postgres`.
Both published ports bind to loopback. The fixed password is for this isolated
development database. These are the same host ports as the Linux experiment;
stop that experiment before starting the Docker version.

```sh
./experiments/feldera_local/run-docker.sh stop
./experiments/feldera_local/run-docker.sh reset
./experiments/feldera_local/run-docker.sh start
```

`stop` preserves the database, runner state, and compiler cache. Since this
trial does not configure fault-tolerant checkpoints, **reset is required after
a stopped or interrupted pipeline startup**. Repeating `start` while the
pipeline is still running simply reports its status. `reset` deletes only the
Docker experiment's database and runner volumes, preserving the compiler cache.
It deletes the source rows you added, so export any data you want to keep first.

Service definitions are in `compose-docker.yml`; `run-docker.sh` handles database
initialization, SQL deployment, readiness checks, and the fresh-snapshot guard.
Edit [program-docker.sql](program-docker.sql) to change the Docker pipeline's
queries or connector settings. It contains the Docker database address directly;
the runner uploads the file without substitution. The Linux runner continues to
use `program.sql`. Runner markers live in the ignored `.tools/feldera-docker`
directory. Do not start the containers directly with Compose after stopping:
that bypasses the fresh-snapshot guard.
No unrelated containers, volumes, or files are removed.

The runner's startup and lifecycle behavior can be tested with Python 3 and `jq`
without Docker:

```sh
python3 -B -m unittest discover -s tests -p 'test_feldera_docker_runner.py'
```

## Linux (native host processes)

This runs two host processes: the repository's PostgreSQL 18 build and a local
Feldera pipeline manager. There are no containers or cloud services. PostgreSQL
publishes `public.events` through logical replication; Feldera snapshots it,
follows changes, incrementally maintains `group_totals`, and writes that view
to `public.feldera_group_totals` in the same PostgreSQL database.

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
On Linux systems with a user systemd manager, `run.sh` starts Feldera as a
temporary user service; its logs are available with `journalctl --user`.

## Try changes

The initial PostgreSQL rows are `(1, east, 10)`, `(2, east, 20)`, and
`(3, west, 7)`. `run.sh status` shows the PostgreSQL `GROUP BY`, Feldera's
materialized view, the PostgreSQL output table, whether the output matches the
source aggregate, and the replication slot's acknowledged LSN.

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

The output connector uses Feldera's `materialized` PostgreSQL mode: it updates
and deletes target rows as the view changes. Its Feldera index on `group_key`
corresponds to the target table's primary key. The target table is not in the
`feldera_events` publication, so Feldera does not read its own output back.
The connector requests an initial snapshot before streaming later changes.

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
[PostgreSQL output connector](https://docs.feldera.com/connectors/sinks/postgresql/),
[native source startup](https://github.com/feldera/feldera#%EF%B8%8F-running-feldera-from-sources),
and [REST API tutorial](https://docs.feldera.com/tutorials/rest_api/).
