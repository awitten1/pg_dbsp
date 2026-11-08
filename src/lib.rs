//! Entry point for the PostgreSQL extension. pgrx turns the declarations here
//! into install SQL: the `dbsp` schema stores the registered source, the
//! worker's durable progress, and the maintained count rows. The SQL-facing
//! functions validate registration and expose worker status. The other
//! modules implement the DBSP circuit, pgoutput decoding, and background
//! worker that keeps those tables current after source transactions commit.

use ::dbsp::{RootCircuit, ZSet};
use pgrx::prelude::*;

mod engine;
mod pgoutput;
mod worker;

// The worker updates these tables in a PostgreSQL transaction before it
// advances its logical replication slot.
pgrx::extension_sql!(
    r#"
CREATE TABLE dbsp.config (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    source_oid oid NOT NULL,
    state text NOT NULL DEFAULT 'initializing',
    applied_lsn pg_lsn,
    last_error text
);
CREATE TABLE dbsp.counts (
    group_key text PRIMARY KEY,
    row_count bigint NOT NULL CHECK (row_count > 0)
);
"#,
    name = "pg_dbsp_tables",
    requires = [dbsp]
);

pgrx::extension_sql!(
    r#"
CREATE FUNCTION dbsp.register_count_view(source regclass) RETURNS boolean
LANGUAGE SQL AS $$
    SELECT dbsp.register_count_view_oid(source::oid::bigint)
$$;
"#,
    name = "pg_dbsp_regclass_api",
    finalize
);

::pgrx::pg_module_magic!(name, version);

#[pg_extern]
fn hello_pg_dbsp() -> &'static str {
    "Hello, pg_dbsp"
}

/// Exercises the pgrx SQL boundary and a single DBSP circuit transaction.
/// This is a smoke check, not a view-maintenance API.
#[pg_extern]
fn dbsp_smoke_test() -> bool {
    let (circuit, (input, output)) = RootCircuit::build(|circuit| {
        let (stream, input) = circuit.add_input_zset::<i64>();
        Ok((input, stream.output()))
    })
    .expect("build DBSP smoke circuit");

    let mut delta = vec![(1_i64, 1_i64).into(), (2_i64, 1_i64).into()];
    input.append(&mut delta);
    circuit.transaction().expect("run DBSP smoke transaction");

    output.consolidate().weighted_count() == 2
}

#[pg_schema]
mod dbsp {
    use pgrx::prelude::*;
    use pgrx::JsonB;

    /// Register the one source table this prototype can maintain. The SQL
    /// wrapper accepts regclass; this Rust function receives its numeric OID.
    #[pg_extern]
    fn register_count_view_oid(source: i64) -> bool {
        if Spi::get_one::<bool>("SELECT current_setting('is_superuser')::boolean")
            .expect("check superuser")
            != Some(true)
        {
            error!("pg_dbsp registration requires a superuser");
        }
        if Spi::get_one::<bool>("SELECT current_setting('wal_level') = 'logical'")
            .expect("check wal_level")
            != Some(true)
        {
            error!("pg_dbsp requires wal_level=logical");
        }
        let oid = u32::try_from(source).expect("source OID out of range");
        let valid = Spi::get_one::<bool>(&format!(
            r#"
            SELECT c.relkind = 'r'
              AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid
                  AND a.attname = 'id' AND a.atttypid = 'bigint'::regtype AND a.attnotnull)
              AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid
                  AND a.attname = 'group_key' AND a.atttypid = 'text'::regtype AND a.attnotnull
                  AND EXISTS (SELECT 1 FROM pg_collation co WHERE co.oid = a.attcollation
                      AND co.collisdeterministic))
              AND EXISTS (SELECT 1 FROM pg_index i JOIN pg_attribute a
                  ON a.attrelid = i.indrelid AND a.attname = 'id'
                  WHERE i.indrelid = c.oid AND i.indisprimary
                    AND i.indnkeyatts = 1 AND i.indkey[0] = a.attnum)
            FROM pg_class c WHERE c.oid = {oid}
        "#
        ))
        .expect("validate source table");
        if valid != Some(true) {
            error!("source must be an ordinary table with id bigint PRIMARY KEY and group_key text NOT NULL using a deterministic collation");
        }
        if Spi::get_one::<bool>("SELECT EXISTS (SELECT 1 FROM dbsp.config)")
            .expect("check registration")
            == Some(true)
        {
            error!("pg_dbsp already has a registered source in this database");
        }
        Spi::run(&format!(
            "INSERT INTO dbsp.config (source_oid) VALUES ({oid})"
        ))
        .expect("register source");
        true
    }

    /// Report the durable LSN alongside live worker presence, since a stored
    /// `ready` state alone cannot show whether the worker has stopped.
    #[pg_extern]
    fn status() -> JsonB {
        let worker_running = Spi::get_one::<bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE backend_type = 'pg_dbsp count worker')",
        )
        .expect("check pg_dbsp worker")
        .unwrap_or(false);
        let row = Spi::connect(|client| {
            let mut rows = client.select(
                "SELECT source_oid::regclass::text, state, applied_lsn::text, last_error FROM dbsp.config",
                Some(1), &[]).expect("read pg_dbsp status");
            rows.next().map(|row| {
                (
                    row.get::<String>(1).expect("source"),
                    row.get::<String>(2).expect("state"),
                    row.get::<String>(3).expect("LSN"),
                    row.get::<String>(4).expect("error"),
                )
            })
        });
        match row {
            Some((source, state, lsn, error)) => JsonB(serde_json::json!({
                "source": source, "state": if worker_running { state.as_deref().unwrap_or("unknown") } else { "stopped" },
                "worker_running": worker_running, "applied_lsn": lsn, "last_error": error
            })),
            None => JsonB(
                serde_json::json!({"state": "unregistered", "worker_running": worker_running}),
            ),
        }
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_hello_pg_dbsp() {
        assert_eq!("Hello, pg_dbsp", crate::hello_pg_dbsp());
    }

    #[pg_test]
    fn test_dbsp_smoke_test() {
        assert_eq!(
            Some(true),
            Spi::get_one::<bool>("SELECT dbsp_smoke_test()").unwrap()
        );
    }

    #[pg_test]
    fn test_count_engine_insert_update_delete() {
        use crate::engine::{CountEngine, RowChange};
        let mut engine = CountEngine::new().unwrap();
        let output = engine
            .apply(&[
                RowChange::Insert {
                    id: 1,
                    group: "a".into(),
                },
                RowChange::Insert {
                    id: 2,
                    group: "a".into(),
                },
                RowChange::Insert {
                    id: 3,
                    group: "b".into(),
                },
            ])
            .unwrap();
        assert!(output.contains(&("a".into(), Some(2))));
        assert!(output.contains(&("b".into(), Some(1))));
        let output = engine
            .apply(&[
                RowChange::Update {
                    old_id: 2,
                    id: 2,
                    group: Some("b".into()),
                },
                RowChange::Delete { id: 3 },
            ])
            .unwrap();
        assert!(output.contains(&("a".into(), Some(1))));
        assert!(!output.iter().any(|(group, _)| group == "b"));
        let output = engine.apply(&[RowChange::Delete { id: 2 }]).unwrap();
        assert!(output.contains(&("b".into(), None)));
    }
}

/// This module is required by `cargo pgrx test` invocations.
/// It must be visible at the root of your extension crate.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {
        // perform one-off initialization when the pg_test framework starts
    }

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        // return any postgresql.conf settings that are required for your tests
        vec![]
    }
}
