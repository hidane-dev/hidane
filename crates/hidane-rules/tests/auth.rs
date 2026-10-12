//! `request.auth` as `tools/oracle/rules_auth.py` measured it: null without a token; with one,
//! `uid` is the token's `sub`, or null when it has none, and `token` holds every claim.

mod common;

use common::{Store, document_path, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{Auth, Budget, Decision, Map, Method, Request, Value, authorize},
};

fn decide(condition: &str, auth: Option<Auth>) -> Decision {
    let content = format!(
        "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n    match /c/{{d}} {{\n      allow read: if {condition};\n    }}\n  }}\n}}\n"
    );
    let ruleset = compile(&[SourceFile {
        name: None,
        content,
    }])
    .ruleset
    .expect("compiles");
    let request = Request {
        upsert: false,
        in_transaction: false,
        write_fields: None,
        transforms: None,
        auto_id: false,
        method: Method::Get,
        path: document_path("c/d"),
        auth,
        time: now(),
        resource: None,
        query: None,
    };
    authorize(
        &ruleset,
        &request,
        None,
        &Store(Vec::new()),
        &mut Budget::single(),
    )
}

fn token(claims: &[(&str, Value)]) -> Map {
    claims
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn denied(message: &str) -> Decision {
    Decision::Deny(format!("\n{message} for 'get' @ L5"))
}

#[test]
fn without_a_token() {
    assert_eq!(decide("request.auth == null", None), Decision::Allow);
    assert_eq!(
        decide("request.auth.uid == 'u1'", None),
        denied("Null value error.")
    );
}

#[test]
fn a_token_with_a_subject() {
    let auth = Auth {
        uid: Some("u1".to_owned()),
        token: token(&[
            ("sub", Value::String("u1".to_owned())),
            ("email_verified", Value::Bool(true)),
            ("exp", Value::Int(1_900_000_000)),
        ]),
    };
    for condition in [
        "request.auth != null",
        "request.auth.uid == 'u1'",
        "request.auth.token.sub == 'u1'",
        "request.auth.token.email_verified == true",
        "request.auth.token.exp is int",
    ] {
        assert_eq!(
            decide(condition, Some(auth.clone())),
            Decision::Allow,
            "{condition}"
        );
    }
    assert_eq!(
        decide("request.auth.token.email == 'a'", Some(auth)),
        denied("Property email is undefined on object.")
    );
}

#[test]
fn a_token_without_a_subject_has_a_null_uid() {
    let auth = Auth {
        uid: None,
        token: token(&[("user_id", Value::String("u1".to_owned()))]),
    };
    assert_eq!(
        decide("request.auth.uid == null", Some(auth.clone())),
        Decision::Allow
    );
    assert_eq!(
        decide("request.auth.uid is string", Some(auth.clone())),
        denied("false")
    );
    assert_eq!(
        decide("request.auth.uid == 'u1'", Some(auth.clone())),
        denied("false")
    );
    assert_eq!(
        decide("request.auth.token.user_id == 'u1'", Some(auth)),
        Decision::Allow
    );
}
