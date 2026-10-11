//! Conditions against the official emulator (`results/rules-conditions-official-v1.22.0.json`,
//! from `tools/oracle/rules_conditions.py`): for each of the 1,451 conditions, the same ruleset
//! is compiled and a `get` of `rooms/r1` without auth is decided, and the outcome must be the
//! recorded one: `allow`, `false` (denied with `false for 'get' @ L5`), `error` (denied with
//! the error the official emulator printed) or `compile` (the ruleset does not compile).
//!
//! The official emulator evaluates a failed condition again and prints every entry twice, and
//! one that reads `resource` once more with an `evaluation error at L5:22` first; hidane
//! evaluates once, so the expected message is the one entry.

mod common;

use std::collections::BTreeMap;

use common::{Store, document_path, fields, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{Budget, Decision, Document, Method, Request, authorize},
};
use serde_json::Value as Json;

/// Conditions whose outcome differs from the official emulator's, and why.
const KNOWN: &[(&str, &str)] = &[];

/// The entry hidane prints for a denial the official emulator recorded as `messages`.
fn expected_entry(messages: &[Json]) -> String {
    let full = messages
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(", ");
    let full = full
        .strip_prefix("evaluation error at L5:22 for 'get' @ L5, ")
        .unwrap_or(&full);
    let half = full.len().saturating_sub(2) / 2;
    if full.len() >= 2
        && full.get(..half) == full.get(half + 2..)
        && full.get(half..half + 2) == Some(", ")
    {
        full[..half].to_owned()
    } else {
        full.to_owned()
    }
}

#[test]
fn conditions_decide_as_on_the_official_emulator() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../results/rules-conditions-official-v1.22.0.json"
    );
    let fixture: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let template = fixture["rules"].as_str().unwrap();
    let document = Document {
        path: document_path("rooms/r1"),
        data: fields(&fixture["document"]),
    };
    let store = Store(vec![document.clone()]);

    // Per group: cases, matches.
    let mut tally: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut failures = Vec::new();
    for result in fixture["results"].as_array().unwrap() {
        let name = result["name"].as_str().unwrap();
        let condition = result["condition"].as_str().unwrap();
        let outcome = result["outcome"].as_str().unwrap();
        let compiled = compile(&[SourceFile {
            name: Some("firestore.rules".to_owned()),
            content: template.replacen("%s", condition, 1),
        }]);
        let got = match &compiled.ruleset {
            None => ("compile".to_owned(), None),
            Some(ruleset) => {
                let request = Request {
                    upsert: false,
                    method: Method::Get,
                    path: document_path("rooms/r1"),
                    auth: None,
                    time: now(),
                    resource: None,
                    query: None,
                };
                let decision = authorize(
                    ruleset,
                    &request,
                    Some(&document),
                    &store,
                    &mut Budget::single(),
                );
                match decision {
                    Decision::Allow => ("allow".to_owned(), None),
                    Decision::Deny(m) if m == "\nfalse for 'get' @ L5" => {
                        ("false".to_owned(), None)
                    }
                    // The oracle split messages at ", " and trimmed each part.
                    Decision::Deny(m) => (
                        "error".to_owned(),
                        Some(
                            m.split(", ")
                                .map(str::trim_end)
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                    ),
                }
            }
        };
        let expected_message = (outcome == "error").then(|| {
            format!(
                "\n{}",
                expected_entry(result["messages"].as_array().unwrap())
            )
        });
        let matches = got.0 == outcome && (outcome != "error" || got.1 == expected_message);
        let group = name.split('/').next().unwrap().to_owned();
        let entry = tally.entry(group).or_default();
        entry.0 += 1;
        if matches {
            entry.1 += 1;
        } else {
            failures.push(format!(
                "{name}: {condition}\n  official: {outcome} {expected_message:?}\n  hidane:   {} {:?}{}",
                got.0,
                got.1,
                compiled
                    .issues
                    .first()
                    .map(|i| format!("\n  issue: {}", i.line()))
                    .unwrap_or_default()
            ));
        }
    }
    for (group, (cases, matched)) in &tally {
        eprintln!("{group}: {matched} of {cases}");
    }
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| {
            !KNOWN
                .iter()
                .any(|(name, _)| f.starts_with(&format!("{name}: ")))
        })
        .collect();
    for f in &unexpected {
        eprintln!("{f}");
    }
    assert!(
        unexpected.is_empty(),
        "{} unexpected results",
        unexpected.len()
    );
    assert_eq!(
        failures.len(),
        KNOWN.len(),
        "an allow-listed case matches now"
    );
}
