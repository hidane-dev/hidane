//! hidane must order values and document names exactly as the official emulator does.
//!
//! `fixtures/value_order.json` was produced by `tools/oracle/value_order.py` against the
//! official emulator v1.22.0: groups of values the emulator considers equal, in ascending order,
//! and document names of a collection group in `__name__` order.

mod common;

use std::cmp::Ordering;

use hidane_core::{
    key::{encode_path, encode_value},
    normalize::normalize_value,
    order::{compare, compare_paths},
};
use hidane_proto::google::firestore::v1::Value;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/value_order.json")).unwrap()
}

/// The oracle's groups as stored values (after write-time normalization), with labels.
fn groups() -> Vec<Vec<(String, Value)>> {
    fixture()["values"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| {
            group
                .as_array()
                .unwrap()
                .iter()
                .map(|case| {
                    let mut value = common::value(&case["value"]);
                    normalize_value(&mut value);
                    (case["label"].as_str().unwrap().to_owned(), value)
                })
                .collect()
        })
        .collect()
}

#[test]
fn values_within_a_group_are_equal() {
    for group in groups() {
        for (label_a, a) in &group {
            for (label_b, b) in &group {
                assert_eq!(compare(a, b), Ordering::Equal, "{label_a} vs {label_b}");
                assert_eq!(
                    encode_value(a),
                    encode_value(b),
                    "{label_a} and {label_b} compare equal but encode differently"
                );
            }
        }
    }
}

#[test]
fn groups_are_strictly_ascending() {
    let all: Vec<(usize, String, Value)> = groups()
        .into_iter()
        .enumerate()
        .flat_map(|(rank, group)| group.into_iter().map(move |(l, v)| (rank, l, v)))
        .collect();
    for (rank_a, label_a, a) in &all {
        for (rank_b, label_b, b) in &all {
            let expected = rank_a.cmp(rank_b);
            assert_eq!(compare(a, b), expected, "compare({label_a}, {label_b})");
            assert_eq!(
                encode_value(a).cmp(&encode_value(b)),
                expected,
                "encode({label_a}) vs encode({label_b})"
            );
        }
    }
}

#[test]
fn document_names_sort_like_a_collection_group_query() {
    let fixture = fixture();
    let expected: Vec<&str> = fixture["paths"]["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();

    let mut by_compare = expected.clone();
    by_compare.reverse();
    by_compare.sort_by(|a, b| compare_paths(a.split('/'), b.split('/')));
    assert_eq!(by_compare, expected);

    let mut by_key = expected.clone();
    by_key.reverse();
    by_key.sort_by_key(|p| encode_path(p.split('/')));
    assert_eq!(by_key, expected);
}
