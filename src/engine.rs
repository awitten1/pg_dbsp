//! DBSP state for one count-by-group view. The circuit accepts weighted source
//! rows and incrementally computes counts for each `group_key`. A separate
//! `id -> group_key` map remembers source rows because pgoutput can send only
//! a primary key for a delete or the old side of an update. The worker feeds
//! one committed source transaction at a time and writes only groups whose
//! counts changed. This state lives in memory and is rebuilt on worker start.

use std::collections::HashMap;

use ::dbsp::utils::Tup2;
use ::dbsp::{CircuitHandle, OrdIndexedZSet, OutputHandle, RootCircuit, ZSetHandle};

pub struct CountEngine {
    circuit: CircuitHandle,
    input: ZSetHandle<Tup2<i64, String>>,
    output: OutputHandle<OrdIndexedZSet<String, i64>>,
    rows: HashMap<i64, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowChange {
    Insert {
        id: i64,
        group: String,
    },
    Update {
        old_id: i64,
        id: i64,
        group: Option<String>,
    },
    Delete {
        id: i64,
    },
}

impl CountEngine {
    /// Build an empty single-threaded circuit for the supported aggregate.
    pub fn new() -> Result<Self, String> {
        let (circuit, (input, output)) = RootCircuit::build(|circuit| {
            let (stream, input) = circuit.add_input_zset::<Tup2<i64, String>>();
            let counts = stream
                .map_index(|row| (row.1.clone(), 1_i64))
                .aggregate_linear(|v| *v);
            Ok((input, counts.output()))
        })
        .map_err(|error| error.to_string())?;
        Ok(Self {
            circuit,
            input,
            output,
            rows: HashMap::new(),
        })
    }

    /// Apply a complete source transaction. `None` means a group disappeared
    /// and should be deleted from `dbsp.counts`; `Some(n)` is its new count.
    pub fn apply(&mut self, changes: &[RowChange]) -> Result<Vec<(String, Option<i64>)>, String> {
        let mut delta = Vec::with_capacity(changes.len() * 2);
        for change in changes {
            match change {
                RowChange::Insert { id, group } => {
                    if self.rows.insert(*id, group.clone()).is_some() {
                        return Err(format!("duplicate source id {id}"));
                    }
                    delta.push(Tup2(Tup2(*id, group.clone()), 1));
                }
                RowChange::Update { old_id, id, group } => {
                    let old_group = self
                        .rows
                        .remove(old_id)
                        .ok_or_else(|| format!("unknown source id {old_id}"))?;
                    let group = group.clone().unwrap_or_else(|| old_group.clone());
                    if old_id != id && self.rows.contains_key(id) {
                        return Err(format!("duplicate replacement id {id}"));
                    }
                    delta.push(Tup2(Tup2(*old_id, old_group), -1));
                    delta.push(Tup2(Tup2(*id, group.clone()), 1));
                    self.rows.insert(*id, group);
                }
                RowChange::Delete { id } => {
                    let old_group = self
                        .rows
                        .remove(id)
                        .ok_or_else(|| format!("unknown source id {id}"))?;
                    delta.push(Tup2(Tup2(*id, old_group), -1));
                }
            }
        }
        self.input.append(&mut delta);
        self.circuit
            .transaction()
            .map_err(|error| error.to_string())?;
        // Aggregation emits a retraction of the old count and an insertion of
        // the new count for each changed group.
        let mut changed = HashMap::new();
        for (group, count, weight) in self.output.consolidate().iter() {
            if weight > 0 {
                changed.insert(group.clone(), Some(count));
            } else {
                changed.entry(group.clone()).or_insert(None);
            }
        }
        Ok(changed.into_iter().collect())
    }
}
