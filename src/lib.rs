use dbsp::{RootCircuit, ZSet};
use pgrx::prelude::*;

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
