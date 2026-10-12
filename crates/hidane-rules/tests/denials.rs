//! What a denial says, for the cases of `tools/oracle/rules_denials.py` that need no server:
//! `false for 'get' @ L5` with the line of the condition, the error instead of `false`, one
//! entry per statement in source order joined by `, `, and `No matching allow statements` when
//! none applies; for a write without a precondition, the statements of both `create` and
//! `update`. The official emulator lists the entries again after an error; hidane lists them
//! once. Batches and queries are left to the server's tests.

mod common;

use common::{document_path, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{Budget, Decision, Document, Documents, Map, Method, Request, Value, authorize},
};

/// `c/d` holds `n: 1`.
struct Store;

fn stored() -> Document {
    Document {
        path: document_path("c/d"),
        data: Map::from([("n".to_owned(), Value::Int(1))]),
    }
}

impl Documents for Store {
    fn get(&self, path: &[String]) -> Option<Document> {
        (path == stored().path).then(stored)
    }

    fn get_after(&self, path: &[String]) -> Option<Document> {
        self.get(path)
    }
}

/// The decision for `method` on `path` under the ruleset of `rules_denials.py` around `body`.
fn decide(body: &str, method: Method, path: &str) -> Decision {
    decide_as(body, method, path, false)
}

/// The same for a write that may be an upsert.
fn decide_as(body: &str, method: Method, path: &str, upsert: bool) -> Decision {
    let indented: Vec<String> = body.lines().map(|l| format!("    {l}")).collect();
    let content = format!(
        "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n{}\n  }}\n}}\n",
        indented.join("\n")
    );
    let ruleset = compile(&[SourceFile {
        name: None,
        content,
    }])
    .ruleset
    .expect("compiles");
    let path = document_path(path);
    let resource = Store.get(&path);
    let new = match method {
        Method::Create | Method::Update => Some(Document {
            path: path.clone(),
            data: Map::from([("n".to_owned(), Value::Int(2))]),
        }),
        _ => None,
    };
    let request = Request {
        upsert,
        in_transaction: false,
        write_fields: None,
        transforms: None,
        auto_id: false,
        method,
        path,
        auth: None,
        time: now(),
        resource: new,
        query: None,
    };
    authorize(
        &ruleset,
        &request,
        resource.as_ref(),
        &Store,
        &mut Budget::single(),
    )
}

fn denied(message: &str) -> Decision {
    Decision::Deny(message.to_owned())
}

#[test]
fn denials_name_each_statement() {
    let get = |body: &str| decide(body, Method::Get, "c/d");
    assert_eq!(
        get("match /c/{d} {\n  allow read: if false;\n}"),
        denied("\nfalse for 'get' @ L5")
    );
    // The line of the condition, not of the statement.
    assert_eq!(
        get("match /c/{d} {\n  allow read: if\n    false;\n}"),
        denied("\nfalse for 'get' @ L6")
    );
    let conversion = "Type conversion error. Argument: [a], From type: [string], To type: [int].";
    assert_eq!(
        get("match /c/{d} {\n  allow read: if int('a') == 1;\n}"),
        denied(&format!("\n{conversion} for 'get' @ L5"))
    );
    // An error in a function is reported at the condition that called it.
    let content =
        "function f() {\n  return int('a') == 1;\n}\nmatch /c/{d} {\n  allow read: if f();\n}";
    assert_eq!(
        get(content),
        denied(&format!("\n{conversion} for 'get' @ L8"))
    );
    assert_eq!(
        get("match /c/{d} {\n  allow read: if resource.data.missing == 1;\n}"),
        denied("\nProperty missing is undefined on object. for 'get' @ L5")
    );
    assert_eq!(
        get("match /c/{d} {\n  allow write: if true;\n}"),
        denied("No matching allow statements")
    );
    assert_eq!(
        get("match /x/{d} {\n  allow read: if true;\n}"),
        denied("No matching allow statements")
    );
    assert_eq!(
        get("match /c/{d} {\n  allow read: if false;\n  allow get: if 1 == 2;\n}"),
        denied("\nfalse for 'get' @ L5, false for 'get' @ L6")
    );
    assert_eq!(
        get(
            "match /c/{d} {\n  allow read: if false;\n}\nmatch /{p=**} {\n  allow read: if false;\n}"
        ),
        denied("\nfalse for 'get' @ L5, false for 'get' @ L8")
    );
    assert_eq!(
        get("match /c/{d} {\n  allow read: if false;\n}\nmatch /x/{d} {\n  allow read;\n}"),
        denied("\nfalse for 'get' @ L5")
    );
    assert_eq!(
        get("match /c/{d} {\n  allow read: if false;\n}\nmatch /{p=**} {\n  allow read;\n}"),
        Decision::Allow
    );
}

#[test]
fn denials_name_the_method() {
    for (method, path, statement) in [
        (Method::Create, "c/new", "create"),
        (Method::Update, "c/d", "update"),
        (Method::Delete, "c/d", "delete"),
    ] {
        let name = method.name();
        assert_eq!(
            decide(
                &format!("match /c/{{d}} {{\n  allow {statement}: if false;\n}}"),
                method,
                path
            ),
            denied(&format!("\nfalse for '{name}' @ L5"))
        );
        assert_eq!(
            decide("match /c/{d} {\n  allow write: if false;\n}", method, path),
            denied(&format!("\nfalse for '{name}' @ L5"))
        );
        assert_eq!(
            decide("match /c/{d} {\n  allow read: if true;\n}", method, path),
            denied("No matching allow statements")
        );
    }
    assert_eq!(
        decide(
            "match /c/{d} {\n  allow list: if false;\n}",
            Method::List,
            "c"
        ),
        denied("\nfalse for 'list' @ L5")
    );
}

/// A write without a precondition is an update if the document exists, else a create; a
/// denial lists the statements of both, `create`'s first, the other method's as `false` or its
/// condition's error (`rules_writes.py`, `rules_denials.py`).
#[test]
fn upserts_list_both_methods() {
    let update = |body: &str| decide_as(body, Method::Update, "c/d", true);
    let create = |body: &str| decide_as(body, Method::Create, "c/new", true);
    assert_eq!(
        update("match /c/{d} {\n  allow write: if false;\n}"),
        denied("\nfalse for 'create' @ L5, false for 'update' @ L5")
    );
    assert_eq!(
        create("match /c/{d} {\n  allow write: if false;\n}"),
        denied("\nfalse for 'create' @ L5, false for 'update' @ L5")
    );
    assert_eq!(
        update("match /c/{d} {\n  allow update: if false;\n  allow create: if true;\n}"),
        denied("\nfalse for 'create' @ L6, false for 'update' @ L5")
    );
    assert_eq!(
        create("match /c/{d} {\n  allow update: if true;\n  allow create: if false;\n}"),
        denied("\nfalse for 'create' @ L6, false for 'update' @ L5")
    );
    assert_eq!(
        update("match /c/{d} {\n  allow create: if true;\n}"),
        denied("\nfalse for 'create' @ L5")
    );
    assert_eq!(
        update("match /c/{d} {\n  allow create: if int('a') == 1;\n}"),
        denied(
            "\nType conversion error. Argument: [a], From type: [string], To type: [int]. \
             for 'create' @ L5"
        )
    );
    assert_eq!(
        update("match /c/{d} {\n  allow delete: if true;\n}"),
        denied("No matching allow statements")
    );
    assert_eq!(
        update("match /c/{d} {\n  allow update: if true;\n  allow create: if false;\n}"),
        Decision::Allow
    );
    // With a precondition, only the method it chose.
    assert_eq!(
        decide(
            "match /c/{d} {\n  allow create: if true;\n}",
            Method::Update,
            "c/d"
        ),
        denied("No matching allow statements")
    );
}
