use dbsp::{RootCircuit, ZSet};
use pgrx::prelude::*;

pgrx::pg_module_magic!();

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
