//! Resident memory of the in-memory store, with the document shape of the Phase 0 baseline
//! (`users/{i}` = `{mykey: "my data", myid: i}`, written 500 per commit).
//!
//! `cargo run --release -p hidane-core --example rss -- 1000000`

use std::{collections::BTreeMap, process::Command, time::Instant};

use hidane_core::{
    path::ResourcePath,
    store::{MemoryStore, Store},
};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};

const DB: &str = "projects/demo/databases/(default)";

fn rss_mib() -> f64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    let kib: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0);
    kib / 1024.0
}

fn main() {
    let total: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(1_000_000);
    let store = MemoryStore::new();
    let checkpoints = [1_000, 100_000, 500_000, 1_000_000];
    println!("documents,rss_mib,elapsed_s");
    println!("0,{:.1},0.000", rss_mib());
    let started = Instant::now();
    let mut written = 0;
    while written < total {
        let batch_end = (written + 500).min(total);
        store
            .commit(DB, &mut |batch| {
                for i in written..batch_end {
                    let path = ResourcePath::from_segments(["users".to_owned(), i.to_string()]);
                    let fields: BTreeMap<String, Value> = [
                        (
                            "mykey".to_owned(),
                            Value {
                                value_type: Some(ValueType::StringValue("my data".to_owned())),
                            },
                        ),
                        (
                            "myid".to_owned(),
                            Value {
                                value_type: Some(ValueType::IntegerValue(
                                    i64::try_from(i).unwrap(),
                                )),
                            },
                        ),
                    ]
                    .into();
                    batch.set(&path, fields);
                }
                Ok(())
            })
            .unwrap();
        written = batch_end;
        if checkpoints.contains(&written) {
            println!(
                "{written},{:.1},{:.3}",
                rss_mib(),
                started.elapsed().as_secs_f64()
            );
        }
    }
}
