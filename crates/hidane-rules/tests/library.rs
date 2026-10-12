//! Evaluation against the official emulator (`fixtures/rules_eval.json`, from
//! `tools/oracle/rules_eval.py`): operators and their errors, conversions, every function and
//! method, lookups, scoping and the entries of a denial. Each case is a condition in the
//! ruleset of `rules_conditions.py`, or a whole ruleset, decided for a `get` of `rooms/r1`
//! without auth; the decision and the denial's message must be the official ones.
//!
//! The official emulator evaluates again after an error and lists every entry twice (the first
//! time, a condition that reads `resource` or a document fails with `evaluation error at …`);
//! hidane evaluates once and lists the entries of the second evaluation.

mod common;

use common::{Store, document_path, fields, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{Budget, Decision, Document, Documents, Map, Method, Request, Value, authorize},
};
use regex::Regex;
use serde_json::Value as Json;

/// Cases where hidane differs, and why.
const KNOWN: &[(&str, &str)] = &[];

/// The message hidane prints for an official denial: the entries of the last evaluation.
fn expected(message: &str) -> String {
    let Some(body) = message.strip_prefix('\n') else {
        return message.to_owned();
    };
    let entry = Regex::new(r"(?s)(.*?) for '[a-z]+' @ L\d+(, |$)").unwrap();
    let entries: Vec<&str> = entry
        .find_iter(body)
        .map(|m| m.as_str().trim_end_matches(", "))
        .collect();
    let taken = if entries.iter().all(|e| e.starts_with("false for ")) {
        &entries[..]
    } else {
        assert!(
            entries.len().is_multiple_of(2),
            "an odd number of entries: {message:?}"
        );
        &entries[entries.len() / 2..]
    };
    format!("\n{}", taken.join(", "))
}

/// Whether every write of a recorded batch is allowed: `c/x` is stored, each write updates
/// (`+id`) or deletes (`-id`) a document of `c`, and the batch's documents are free to
/// look up.
fn batch(result: &Json) -> bool {
    let content = format!(
        "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n    match /c/{{d}} {{\n      allow read: if true;\n      allow write: if {};\n    }}\n  }}\n}}\n",
        result["condition"].as_str().unwrap()
    );
    let ruleset = compile(&[SourceFile {
        name: None,
        content,
    }])
    .ruleset
    .unwrap();
    let doc = |id: &str, n| Document {
        path: document_path(&format!("c/{id}")),
        data: Map::from([("n".to_owned(), Value::Int(n))]),
    };
    let writes: Vec<(bool, &str)> = result["writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            let w = w.as_str().unwrap();
            (w.starts_with('+'), &w[1..])
        })
        .collect();
    let before = Store(vec![doc("x", 1)]);
    let mut after: Vec<Document> = before
        .0
        .iter()
        .filter(|d| !writes.iter().any(|(_, id)| d.path == doc(id, 0).path))
        .cloned()
        .collect();
    after.extend(
        writes
            .iter()
            .filter(|(update, _)| *update)
            .map(|(_, id)| doc(id, 2)),
    );
    struct Batch(Store, Store);
    impl Documents for Batch {
        fn get(&self, path: &[String]) -> Option<Document> {
            self.0.get(path)
        }
        fn get_after(&self, path: &[String]) -> Option<Document> {
            self.1.get(path)
        }
    }
    let documents = Batch(before, Store(after));
    let mut budget = Budget::batch();
    for (_, id) in &writes {
        budget.add_written(document_path(&format!("c/{id}")));
    }
    writes.iter().all(|(update, id)| {
        budget.next_write();
        let stored = documents.get(&document_path(&format!("c/{id}")));
        let method = match (update, &stored) {
            (false, _) => Method::Delete,
            (true, Some(_)) => Method::Update,
            (true, None) => Method::Create,
        };
        let request = Request {
            upsert: false,
            in_transaction: false,
            write_fields: None,
            transforms: None,
            auto_id: false,
            verify: false,
            method,
            path: document_path(&format!("c/{id}")),
            auth: None,
            time: now(),
            resource: update.then(|| doc(id, 2)),
            query: None,
        };
        authorize(&ruleset, &request, stored.as_ref(), &documents, &mut budget) == Decision::Allow
    })
}

#[test]
fn evaluation_matches_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/rules_eval.json")).unwrap();
    let template = fixture["rules"].as_str().unwrap();
    let document = Document {
        path: document_path("rooms/r1"),
        data: fields(&fixture["document"]),
    };
    let other = Document {
        path: document_path("rooms/r2"),
        data: Map::from([("n".to_owned(), Value::Int(1))]),
    };
    let store = Store(vec![document.clone(), other]);
    let mut failures = Vec::new();
    let mut compared = 0;
    for result in fixture["results"].as_array().unwrap() {
        let name = result["name"].as_str().unwrap();
        if name.starts_with("batches/") {
            compared += 1;
            let want = result["outcome"] == "allow";
            if batch(result) != want {
                failures.push(format!("{name}: official allows: {want}"));
            }
            continue;
        }
        let content = match result.get("condition") {
            Some(condition) => template.replacen("%s", condition.as_str().unwrap(), 1),
            None => result["rules"].as_str().unwrap().to_owned(),
        };
        let compiled = compile(&[SourceFile {
            name: Some("firestore.rules".to_owned()),
            content,
        }]);
        // `Err` for a ruleset that does not compile, with the message.
        let got = match &compiled.ruleset {
            None => Err(compiled.error_message()),
            Some(ruleset) => {
                let request = Request {
                    upsert: false,
                    in_transaction: false,
                    write_fields: None,
                    transforms: None,
                    auto_id: false,
                    verify: false,
                    method: Method::Get,
                    path: document_path("rooms/r1"),
                    auth: None,
                    time: now(),
                    resource: None,
                    query: None,
                };
                Ok(authorize(
                    ruleset,
                    &request,
                    Some(&document),
                    &store,
                    &mut Budget::single(),
                ))
            }
        };
        let message = || result["message"].as_str().unwrap().to_owned();
        let want = match &result["outcome"] {
            o if o == "allow" => Ok(Decision::Allow),
            o if o == "deny" => Ok(Decision::Deny(expected(&message()))),
            o if o == "compile" => Err(message()),
            // A 500 of the official emulator is an evaluation error here
            // (`docs/parity-exceptions.md`).
            _ if matches!(got, Ok(Decision::Deny(_))) => got.clone(),
            other => Err(format!("status {other}")),
        };
        compared += 1;
        if got != want && !KNOWN.iter().any(|(n, _)| *n == name) {
            failures.push(format!(
                "{name}: {}\n  official: {want:?}\n  hidane:   {got:?}",
                result
                    .get("condition")
                    .and_then(Json::as_str)
                    .unwrap_or("(ruleset)")
            ));
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    assert!(failures.is_empty(), "{} cases differ", failures.len());
    assert_eq!(compared, 1278);
}
