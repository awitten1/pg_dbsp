//! PostgreSQL background worker and recovery loop. `_PG_init` registers one
//! worker at server startup for the configured database. After registration,
//! the worker creates a publication and logical slot, locks and scans the
//! source, and loads an empty DBSP circuit. It peeks committed pgoutput changes
//! without consuming them, commits changed counts with an applied LSN, then
//! advances the slot. On restart, truncation, or apply failure it rebuilds
//! from a locked snapshot rather than restoring an in-memory checkpoint.

use std::ffi::CString;
use std::time::Duration;

use pgrx::bgworkers::{BackgroundWorker, BackgroundWorkerBuilder, SignalWakeFlags};
use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use pgrx::prelude::*;

use crate::engine::{CountEngine, RowChange};
use crate::pgoutput::{from_hex, lsn_string, Decoder, Message};

const PUBLICATION: &str = "pg_dbsp_count_pub";
const SLOT: &str = "pg_dbsp_count_slot";
static DATABASE: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(Some(c"postgres"));
static TEST_PAUSE_AFTER_APPLY_MS: GucSetting<i32> = GucSetting::<i32>::new(0);

#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    GucRegistry::define_string_guc(
        c"pg_dbsp.database",
        c"Database for the pg_dbsp count worker",
        c"The first prototype maintains one source table in this database.",
        &DATABASE,
        GucContext::Sighup,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pg_dbsp.test_pause_after_apply_ms",
        c"Pause after a durable count apply",
        c"Fault injection for the slot replay integration test; keep zero outside tests.",
        &TEST_PAUSE_AFTER_APPLY_MS,
        0,
        30_000,
        GucContext::Sighup,
        GucFlags::default(),
    );
    if unsafe { pg_sys::process_shared_preload_libraries_in_progress } {
        BackgroundWorkerBuilder::new("pg_dbsp count worker")
            .set_library("pg_dbsp")
            .set_function("pg_dbsp_worker_main")
            .enable_spi_access()
            .set_restart_time(Some(Duration::from_secs(5)))
            .load();
    }
}

#[pg_guard]
#[no_mangle]
pub extern "C-unwind" fn pg_dbsp_worker_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGTERM | SignalWakeFlags::SIGHUP);
    let database = DATABASE
        .get()
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .unwrap_or("postgres")
        .to_owned();
    BackgroundWorker::connect_worker_to_spi(Some(&database), None);
    let mut engine: Option<CountEngine> = None;
    let mut decoder = Decoder::default();
    let mut peek_limit = 1000;
    loop {
        if !BackgroundWorker::worker_continue() {
            break;
        }
        let mut did_work = false;
        let source = BackgroundWorker::transaction(registered_source);
        match source {
            None => {
                engine = None;
            }
            Some(oid) => {
                if engine.is_none() {
                    match initialize(oid) {
                        Ok(new_engine) => {
                            engine = Some(new_engine);
                            decoder = Decoder::default();
                        }
                        Err(error) => {
                            record_error(&error);
                            warning!("pg_dbsp initialization failed: {error}");
                        }
                    }
                } else if let Some(ref mut current) = engine {
                    match tick(oid, current, &mut decoder, &mut peek_limit) {
                        Ok(processed) => did_work = processed,
                        Err(error) => {
                            record_error(&error);
                            warning!("pg_dbsp worker will rebuild: {error}");
                            engine = None;
                        }
                    }
                }
            }
        }
        if did_work {
            continue;
        }
        if !BackgroundWorker::wait_latch(Some(Duration::from_secs(1))) {
            break;
        }
    }
}

fn registered_source() -> Option<u32> {
    if Spi::get_one::<bool>("SELECT to_regclass('dbsp.config') IS NOT NULL")
        .ok()
        .flatten()
        != Some(true)
    {
        return None;
    }
    Spi::get_one::<i64>("SELECT source_oid::bigint FROM dbsp.config")
        .ok()
        .flatten()
        .map(|oid| oid as u32)
}

fn source_name(oid: u32) -> Result<String, String> {
    Spi::get_one::<String>(&format!(
        "SELECT quote_ident(n.nspname) || '.' || quote_ident(c.relname) \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = {oid}"
    ))
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "registered source table no longer exists".into())
}

fn ensure_capture(oid: u32) -> Result<(), String> {
    let relation = BackgroundWorker::transaction(|| source_name(oid))?;
    let publication_exists = BackgroundWorker::transaction(|| {
        Spi::get_one::<bool>(&format!(
            "SELECT EXISTS (SELECT 1 FROM pg_publication WHERE pubname = '{PUBLICATION}')"
        ))
        .map_err(|error| error.to_string())
    })? == Some(true);
    if !publication_exists {
        BackgroundWorker::transaction(|| {
            Spi::run(&format!(
                "CREATE PUBLICATION {PUBLICATION} FOR TABLE {relation}"
            ))
            .map_err(|error| error.to_string())
        })?;
    }
    let slot_exists = BackgroundWorker::transaction(|| {
        Spi::get_one::<bool>(&format!(
            "SELECT EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = '{SLOT}')"
        ))
        .map_err(|error| error.to_string())
    })? == Some(true);
    if !slot_exists {
        BackgroundWorker::transaction(|| {
            Spi::connect(|client| {
                client.select(
                    &format!("SELECT slot_name::text FROM pg_create_logical_replication_slot('{SLOT}', 'pgoutput')"),
                    Some(1), &[],
                ).map_err(|error| error.to_string()).map(|_| ())
            })
        })?;
    }
    Ok(())
}

// The SHARE lock excludes source writes while the snapshot, count table, and
// WAL boundary become one consistent bootstrap point.
fn initialize(oid: u32) -> Result<CountEngine, String> {
    ensure_capture(oid)?;
    let (engine, boundary) = BackgroundWorker::transaction(|| -> Result<_, String> {
        let relation = source_name(oid)?;
        Spi::run(&format!("LOCK TABLE {relation} IN SHARE MODE"))
            .map_err(|error| error.to_string())?;
        validate_source(oid)?;
        let source_rows = Spi::connect(|client| -> Result<Vec<RowChange>, String> {
            client
                .select(&format!("SELECT id, group_key FROM {relation}"), None, &[])
                .map_err(|error| error.to_string())?
                .map(|row| {
                    Ok(RowChange::Insert {
                        id: row
                            .get::<i64>(1)
                            .map_err(|error| error.to_string())?
                            .ok_or("NULL source id")?,
                        group: row
                            .get::<String>(2)
                            .map_err(|error| error.to_string())?
                            .ok_or("NULL group_key")?,
                    })
                })
                .collect()
        })?;
        let mut engine = CountEngine::new()?;
        let output = engine.apply(&source_rows)?;
        Spi::run("TRUNCATE dbsp.counts").map_err(|error| error.to_string())?;
        write_output(&output)?;
        let boundary = Spi::get_one::<String>("SELECT pg_current_wal_lsn()::text")
            .map_err(|error| error.to_string())?
            .ok_or("no WAL LSN")?;
        Spi::run_with_args(
            "UPDATE dbsp.config SET state = 'ready', last_error = NULL, applied_lsn = $1::pg_lsn",
            &[boundary.clone().into()],
        )
        .map_err(|error| error.to_string())?;
        Ok((engine, boundary))
    })?;
    advance_slot(&boundary)?;
    Ok(engine)
}

fn validate_source(oid: u32) -> Result<(), String> {
    let valid = Spi::get_one::<bool>(&format!(
        "SELECT c.relkind = 'r' \
         AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid \
           AND a.attname = 'id' AND a.atttypid = 'bigint'::regtype AND a.attnotnull) \
         AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid \
           AND a.attname = 'group_key' AND a.atttypid = 'text'::regtype AND a.attnotnull \
           AND EXISTS (SELECT 1 FROM pg_collation co WHERE co.oid = a.attcollation \
             AND co.collisdeterministic)) \
         AND EXISTS (SELECT 1 FROM pg_index i JOIN pg_attribute a \
           ON a.attrelid = i.indrelid AND a.attname = 'id' \
           WHERE i.indrelid = c.oid AND i.indisprimary \
             AND i.indnkeyatts = 1 AND i.indkey[0] = a.attnum) \
         FROM pg_class c WHERE c.oid = {oid}"
    ))
    .map_err(|error| error.to_string())?;
    if valid == Some(true) {
        Ok(())
    } else {
        Err("registered source schema is no longer compatible".into())
    }
}

fn write_output(output: &[(String, Option<i64>)]) -> Result<(), String> {
    for (group, count) in output {
        match count {
            Some(count) if *count > 0 => Spi::run_with_args(
                "INSERT INTO dbsp.counts VALUES ($1, $2) \
                 ON CONFLICT (group_key) DO UPDATE SET row_count = EXCLUDED.row_count",
                &[group.clone().into(), (*count).into()],
            )
            .map_err(|error| error.to_string())?,
            _ => Spi::run_with_args(
                "DELETE FROM dbsp.counts WHERE group_key = $1",
                &[group.clone().into()],
            )
            .map_err(|error| error.to_string())?,
        }
    }
    Ok(())
}

fn advance_slot(lsn: &str) -> Result<(), String> {
    BackgroundWorker::transaction(|| {
        Spi::get_one_with_args::<String>(
            &format!("SELECT end_lsn::text FROM pg_replication_slot_advance('{SLOT}', $1::pg_lsn)"),
            &[lsn.to_owned().into()],
        )
        .map_err(|error| error.to_string())
        .map(|_| ())
    })
}

fn peek(limit: i32) -> Result<Vec<Vec<u8>>, String> {
    BackgroundWorker::transaction(|| {
        Spi::connect(|client| {
            client
                .select(
                    &format!(
                        "SELECT encode(data, 'hex') FROM pg_logical_slot_peek_binary_changes(\
                    '{SLOT}', NULL, {limit}, 'proto_version', '1', \
                    'publication_names', '{PUBLICATION}')"
                    ),
                    None,
                    &[],
                )
                .map_err(|error| error.to_string())?
                .map(|row| {
                    let hex = row
                        .get::<String>(1)
                        .map_err(|error| error.to_string())?
                        .ok_or("NULL pgoutput message")?;
                    from_hex(&hex)
                })
                .collect()
        })
    })
}

// Process at most one committed transaction per call. Returning true lets the
// outer loop drain a backlog immediately; it sleeps only when the slot is idle.
fn tick(
    oid: u32,
    engine: &mut CountEngine,
    decoder: &mut Decoder,
    limit: &mut i32,
) -> Result<bool, String> {
    BackgroundWorker::transaction(|| validate_source(oid))?;
    let messages = peek(*limit)?;
    if messages.is_empty() {
        return Ok(false);
    }
    let mut changes = Vec::new();
    let mut in_transaction = false;
    for bytes in messages {
        match decoder.parse(&bytes, oid)? {
            Message::Begin => {
                in_transaction = true;
                changes.clear();
            }
            Message::Change(change) => {
                if !in_transaction {
                    return Err("change outside transaction".into());
                }
                changes.push(change);
            }
            Message::Truncate => return Err("source was truncated; rebuilding".into()),
            Message::Commit(end_lsn) => {
                let lsn = lsn_string(end_lsn);
                let already_applied = BackgroundWorker::transaction(|| {
                    Spi::get_one_with_args::<bool>(
                        "SELECT $1::pg_lsn <= applied_lsn FROM dbsp.config",
                        &[lsn.clone().into()],
                    )
                    .map_err(|error| error.to_string())
                })? == Some(true);
                if already_applied {
                    advance_slot(&lsn)?;
                    *limit = 1000;
                    return Ok(true);
                }
                let output = engine.apply(&changes)?;
                BackgroundWorker::transaction(|| -> Result<(), String> {
                    write_output(&output)?;
                    Spi::run_with_args(
                        "UPDATE dbsp.config SET state = 'ready', last_error = NULL, applied_lsn = $1::pg_lsn",
                        &[lsn.clone().into()],
                    ).map_err(|error| error.to_string())?;
                    Ok(())
                })?;
                let pause_ms = TEST_PAUSE_AFTER_APPLY_MS.get();
                if pause_ms > 0
                    && !BackgroundWorker::wait_latch(Some(Duration::from_millis(pause_ms as u64)))
                {
                    return Ok(false);
                }
                advance_slot(&lsn)?;
                *limit = 1000;
                return Ok(true);
            }
            Message::Other => {}
        }
    }
    *limit = limit
        .checked_mul(2)
        .filter(|value| *value <= 100_000)
        .ok_or("source transaction exceeds 100,000 pgoutput messages")?;
    Ok(true)
}

fn record_error(message: &str) {
    let _ = BackgroundWorker::transaction(|| {
        Spi::run_with_args(
            "UPDATE dbsp.config SET state = 'error', last_error = $1",
            &[message.to_owned().into()],
        )
    });
}
