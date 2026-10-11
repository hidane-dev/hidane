//! `get()`, `exists()` and their `After` forms, and how many documents a request may look up,
//! as `tools/oracle/rules_lookups.py` measured them: a single read or write may look up 10
//! distinct documents and a batch 10 per write and 20 in all; the next lookup is a `Service
//! call error`, and a document looked up again costs nothing. The requests are built here; the
//! test of the recorded answers end to end comes with the server.

mod common;

use common::{document_path, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{Budget, Decision, Document, Documents, Map, Method, Request, Value, authorize},
};

/// The rulesets of `rules_lookups.py`: `body` in `match /databases/{database}/documents`.
fn rules(body: &str) -> hidane_rules::Ruleset {
    let indented: Vec<String> = body.lines().map(|l| format!("    {l}")).collect();
    let content = format!(
        "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n{}\n  }}\n}}\n",
        indented.join("\n")
    );
    let compiled = compile(&[SourceFile {
        name: None,
        content,
    }]);
    compiled.ruleset.expect("compiles")
}

const P: &str = "/databases/$(database)/documents";

fn none_exist(n: usize, distinct: bool) -> String {
    (0..n)
        .map(|i| format!("!exists({P}/missing/m{})", if distinct { i } else { 0 }))
        .collect::<Vec<_>>()
        .join(" && ")
}

/// `!exists()` of `n` documents of the write's own, `missing/<d>_<i>`.
fn own(n: usize) -> String {
    (0..n)
        .map(|i| format!("!exists({P}/missing/$(d + '_{i}'))"))
        .collect::<Vec<_>>()
        .join(" && ")
}

fn doc(path: &str, n: i64) -> Document {
    Document {
        path: document_path(path),
        data: Map::from([("n".to_owned(), Value::Int(n))]),
    }
}

/// Documents before the request and after its writes.
struct Store {
    before: Vec<Document>,
    after: Vec<Document>,
}

impl Documents for Store {
    fn get(&self, path: &[String]) -> Option<Document> {
        self.before.iter().find(|d| d.path == path).cloned()
    }

    fn get_after(&self, path: &[String]) -> Option<Document> {
        self.after.iter().find(|d| d.path == path).cloned()
    }
}

fn store() -> Store {
    let docs = vec![doc("c/d", 1), doc("other/o", 1), doc("c/after", 1)];
    Store {
        before: docs.clone(),
        after: docs,
    }
}

fn request(method: Method, path: &str, resource: Option<Document>) -> Request {
    Request {
        upsert: false,
        method,
        path: document_path(path),
        auth: None,
        time: now(),
        resource,
        query: None,
    }
}

fn read(ruleset: &hidane_rules::Ruleset, documents: &dyn Documents) -> Decision {
    let stored = documents.get(&document_path("c/d"));
    authorize(
        ruleset,
        &request(Method::Get, "c/d", None),
        stored.as_ref(),
        documents,
        &mut Budget::single(),
    )
}

/// An update of `path` in a batch (or alone), checked against `budget`.
fn write(
    ruleset: &hidane_rules::Ruleset,
    path: &str,
    documents: &dyn Documents,
    budget: &mut Budget,
) -> Decision {
    let new = doc(path, 1);
    authorize(
        ruleset,
        &request(Method::Update, path, Some(new)),
        documents.get(&document_path(path)).as_ref(),
        documents,
        budget,
    )
}

/// The official `Service call error` for a lookup of `missing/<id>`.
fn service_error(function: &str, id: &str, method: &str, line: u32) -> Decision {
    let segments: String = ["databases", "(default)", "documents", "missing", id]
        .iter()
        .map(|s| format!("  segments {{\n    simple: \"{s}\"\n  }}\n"))
        .collect();
    Decision::Deny(format!(
        "\nService call error. Function: [{function}], Argument: [path_value {{\n{segments}}}\n]. for '{method}' @ L{line}"
    ))
}

#[test]
fn a_read_may_look_up_ten_documents() {
    let store = store();
    let at = |n, distinct| {
        let ruleset = rules(&format!(
            "match /c/{{d}} {{\n  allow read: if {};\n}}",
            none_exist(n, distinct)
        ));
        read(&ruleset, &store)
    };
    assert_eq!(at(10, true), Decision::Allow);
    // The eleventh fails, and so does every one after it: the first error is reported.
    for n in [11, 20, 21] {
        assert_eq!(at(n, true), service_error("exists", "m10", "get", 5), "{n}");
    }
    assert_eq!(at(15, false), Decision::Allow);
}

#[test]
fn a_write_may_look_up_ten_documents() {
    let store = store();
    for (n, allowed) in [(10, true), (11, false), (21, false)] {
        let ruleset = rules(&format!(
            "match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {};\n}}",
            none_exist(n, true)
        ));
        let got = write(&ruleset, "c/w1", &store, &mut Budget::single());
        let want = if allowed {
            Decision::Allow
        } else {
            service_error("exists", "m10", "update", 6)
        };
        assert_eq!(got, want, "{n}");
    }
}

#[test]
fn a_batch_may_look_up_ten_documents_per_write_and_twenty_in_all() {
    let store = store();
    let batch = |condition: String, writes: usize| -> Vec<Decision> {
        let ruleset = rules(&format!(
            "match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {condition};\n}}"
        ));
        let mut budget = Budget::batch();
        (0..writes)
            .map(|k| {
                budget.next_write();
                write(&ruleset, &format!("c/w{k}"), &store, &mut budget)
            })
            .collect()
    };
    // The same documents in each write: the second looks up nothing new.
    assert_eq!(
        batch(none_exist(10, true), 2),
        [Decision::Allow, Decision::Allow]
    );
    assert_eq!(
        batch(none_exist(11, true), 1),
        [service_error("exists", "m10", "update", 6)]
    );
    // Documents of their own: 20 in all pass, the 11th of one write does not.
    assert_eq!(batch(own(10), 2), [Decision::Allow, Decision::Allow]);
    assert_eq!(
        batch(own(11), 1),
        [service_error("exists", "w0_10", "update", 6)]
    );
    assert_eq!(batch(own(5), 4), vec![Decision::Allow; 4]);
    let six = batch(own(6), 4);
    assert_eq!(six[..3], vec![Decision::Allow; 3]);
    assert_eq!(six[3], service_error("exists", "w3_2", "update", 6));
}

#[test]
fn documents_before_and_after_the_writes() {
    let store = store();
    let allowed = |body: &str| read(&rules(body), &store) == Decision::Allow;
    assert!(allowed(&format!(
        "match /c/{{d}} {{\n  allow read: if get({P}/other/o).data.n == 1;\n}}"
    )));
    assert!(allowed(&format!(
        "match /c/{{d}} {{\n  allow read: if exists({P}/other/o);\n}}"
    )));
    assert!(allowed(&format!(
        "match /c/{{d}} {{\n  allow read: if get({P}/missing/x).data.n == 1 || true;\n}}"
    )));
    assert!(allowed(&format!(
        "function n(id) {{\n  return get({P}/other/$(id)).data.n;\n}}\nmatch /c/{{d}} {{\n  allow read: if n('o') == 1;\n}}"
    )));
    assert!(allowed(&format!(
        "match /c/{{d}} {{\n  allow read: if getAfter({P}/other/o).data.n == 1;\n}}"
    )));
    // A missing document is null.
    assert_eq!(
        read(
            &rules(&format!(
                "match /c/{{d}} {{\n  allow read: if get({P}/missing/x).data.n == 1;\n}}"
            )),
            &store
        ),
        Decision::Deny("\nNull value error. for 'get' @ L5".to_owned())
    );

    // A batch that writes `c/after` (n = 2) and deletes `other/o`.
    let mut after = store.after.clone();
    after.retain(|d| d.path != document_path("other/o") && d.path != document_path("c/after"));
    after.push(doc("c/after", 2));
    let batch = Store {
        before: store.before.clone(),
        after,
    };
    let check = |condition: &str| {
        let ruleset = rules(&format!(
            "match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {condition};\n}}"
        ));
        write(&ruleset, "c/after", &batch, &mut Budget::single())
    };
    assert_eq!(
        check(&format!("getAfter({P}/c/$(d)).data.n == 2")),
        Decision::Allow
    );
    assert_eq!(
        check(&format!("get({P}/c/$(d)).data.n == 1")),
        Decision::Allow
    );
    assert_eq!(
        check(&format!("!existsAfter({P}/other/o)")),
        Decision::Allow
    );
    assert_eq!(
        check(&format!("get({P}/other/o).data.n == 1")),
        Decision::Allow
    );
}

#[test]
fn only_documents_of_the_database_can_be_looked_up() {
    let store = store();
    let at = |path: &str| {
        read(
            &rules(&format!(
                "match /c/{{d}} {{\n  allow read: if exists({path}) || true;\n}}"
            )),
            &store,
        )
    };
    // Absorbed errors: each lookup fails, the condition is still true.
    for path in [
        "/databases/$(database)/documents/c",
        "/databases/other/documents/c/d",
        "/c/d",
    ] {
        assert_eq!(at(path), Decision::Allow, "{path}");
    }
    let denied = read(
        &rules("match /c/{d} {\n  allow read: if exists(/databases/$(database)/documents/c);\n}"),
        &store,
    );
    let Decision::Deny(message) = denied else {
        panic!("allowed")
    };
    assert!(message.starts_with("\nService call error. Function: [exists]"));
    // A failed lookup does not count against the budget.
    let ruleset = rules(&format!(
        "match /c/{{d}} {{\n  allow read: if (exists(/c/d) || true) && {};\n}}",
        none_exist(10, true)
    ));
    assert_eq!(read(&ruleset, &store), Decision::Allow);
}
