//! Conditions nested as deep as the compiler allows are evaluated within the stack of a server
//! thread (2 MiB, tokio's default), with every value traced for the coverage report as the
//! server does; deeper nesting through function calls is an error, not a stack overflow. The
//! type checker, which checks a function's body again for each type its calls expect, takes
//! time in proportion to the functions, not to the calls.

mod common;

use common::{Store, document_path, now};
use hidane_rules::{
    SourceFile, compile,
    coverage::Coverage,
    eval::{Budget, Decision, Method, Request, Trace, authorize_traced},
};

const STACK: usize = 2 * 1024 * 1024;

fn decide(content: String) -> Decision {
    let ruleset = compile(&[SourceFile {
        name: None,
        content: content.clone(),
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
            let mut trace = Trace::with_values();
            let decision = authorize_traced(
                &ruleset,
                &request,
                None,
                &Store(Vec::new()),
                &mut Budget::single(),
                &mut trace,
            );
            let mut coverage = Coverage::new(&ruleset, &content);
            coverage.record(&trace);
            assert!(!coverage.roots().is_empty());
            decision
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

/// Functions that each call the next `calls` times, each call expecting another type of the
/// result and of the arguments; the last one calls the first if `cycle`.
fn chain(functions: usize, calls: usize, cycle: bool) -> String {
    let uses = [
        "f(x, y)",
        "f(y, x).size() == 1",
        "(f(x, 1) + 1).lower() == ''",
        "f(y, y).keys() == []",
        "-f(1, x) == 1",
        "f(x, [x]) in [x]",
        "f([y], x)[0] == x",
        "f(x, y) == f(y, x)",
    ];
    let mut out = String::new();
    for i in 0..functions {
        let next = match (i + 1 < functions, cycle) {
            (true, _) => Some(i + 1),
            (false, true) => Some(0),
            (false, false) => None,
        };
        let result = match next {
            Some(next) => uses[..calls]
                .iter()
                .map(|u| u.replace("f(", &format!("f{next}(")))
                .collect::<Vec<_>>()
                .join(" && "),
            None => "x.size() > 0 ? x : y".to_owned(),
        };
        out.push_str(&format!(
            "  function f{i}(x, y) {{ let a = x; let b = a; return b == y ? {result} : a; }}\n"
        ));
    }
    out
}

/// A chain as deep as the compiler accepts (21 functions), and a longer one and a cycle, which
/// are errors, of functions that each call the next 8 times: each function's signature is
/// worked out once per expected type, so the time grows with the functions, not the calls.
#[test]
fn type_checking_calls_is_bounded() {
    for (functions, cycle, errors) in [(21, false, false), (500, false, true), (500, true, true)] {
        let start = std::time::Instant::now();
        let compiled = compile(&[SourceFile {
            name: None,
            content: rules(&chain(functions, 8, cycle), "f0(1, 'a') && f0('a', [1])"),
        }]);
        assert_eq!(compiled.has_errors(), errors, "{functions} functions");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{functions} functions{}: {:?}",
            if cycle { " in a cycle" } else { "" },
            start.elapsed()
        );
    }
}
