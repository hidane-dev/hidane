//! Conditions nested as deep as the compiler allows are evaluated within the stack of a server
//! thread (2 MiB, tokio's default); deeper nesting through function calls is an error, not a
//! stack overflow.

mod common;

use common::{Store, document_path, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{Budget, Decision, Method, Request, authorize},
};

const STACK: usize = 2 * 1024 * 1024;

fn decide(content: String) -> Decision {
    let ruleset = compile(&[SourceFile {
        name: None,
        content,
    }])
    .ruleset
    .expect("compiles");
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            let request = Request {
                upsert: false,
                method: Method::Get,
                path: document_path("c/d"),
                auth: None,
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
        })
        .unwrap()
        .join()
        .unwrap()
}

fn rules(functions: &str, condition: &str) -> String {
    format!(
        "rules_version = '2';\nservice cloud.firestore {{\n{functions}  match /databases/{{database}}/documents {{\n    match /c/{{d}} {{\n      allow read: if {condition};\n    }}\n  }}\n}}\n"
    )
}

#[test]
fn the_deepest_expression_the_compiler_takes() {
    let chain = format!("{{'a': 1}}{} == 1", ".a".repeat(1_990));
    assert_eq!(
        decide(rules("", &chain)),
        Decision::Deny(
            "\nType error. Received: [int] Expected: [map,path]. for 'get' @ L5".to_owned()
        )
    );
}

#[test]
fn nesting_through_calls_is_bounded() {
    // 20 functions, each reading 150 fields of what the next returns.
    let mut functions = String::new();
    for i in 0..20 {
        let inner = if i == 19 {
            "{'a': 1}".to_owned()
        } else {
            format!("f{}()", i + 1)
        };
        functions.push_str(&format!(
            "  function f{i}() {{ return {inner}{}; }}\n",
            ".a".repeat(150)
        ));
    }
    let Decision::Deny(message) = decide(rules(&functions, "f0() == 1")) else {
        panic!("allowed")
    };
    assert_eq!(
        message,
        "\nExpression is too complex to evaluate safely. for 'get' @ L25"
    );
}
