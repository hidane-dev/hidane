//! The compiler's warnings against the official emulator's (`fixtures/rules_warnings.json`, from
//! `tools/oracle/rules_warnings.py`): every condition and ruleset recorded by the other rules
//! scripts, and cases for the type checker, compiled as `PUT :securityRules` compiles them. The
//! `issues[]` of a ruleset that compiles must be the recorded ones, in order, with their
//! positions and severities; for one that does not, the error message must be the recorded one.

use std::collections::BTreeMap;

use hidane_rules::{Severity, SourceFile, compile};
use serde_json::{Value as Json, json};

/// Syntax errors whose message matches in its first line only: the lines after it come from
/// ANTLR's error recovery, which hidane follows only where it is simple
/// (`docs/parity-exceptions.md`).
const FIRST_LINE_ONLY: &[&str] = &[
    "conditions/cel/basic/self_eval_zeroish/self_eval_string_raw_prefix",
    "conditions/cel-self/basic/self_eval_zeroish/self_eval_string_raw_prefix",
    "conditions/cel/basic/self_eval_nonzeroish/self_eval_int_hex",
    "conditions/cel-self/basic/self_eval_nonzeroish/self_eval_int_hex",
    "conditions/cel/basic/self_eval_nonzeroish/self_eval_int_hex_negative",
    "conditions/cel-self/basic/self_eval_nonzeroish/self_eval_int_hex_negative",
    "conditions/cel/comparisons/eq_literal/eq_raw_string",
    "conditions/cel-self/comparisons/eq_literal/eq_raw_string",
    "conditions/cel/conversions/int/double_range",
    "conditions/cel/parse/receiver_function_names/function",
    "conditions/cel-self/parse/receiver_function_names/function",
    "conditions/cel/parse/receiver_function_names/import",
    "conditions/cel-self/parse/receiver_function_names/import",
];

/// Cases whose issues differ from the official emulator's, and why.
const KNOWN: &[(&str, &str)] = &[
    (
        "conditions/cel/integer_math/int64_math/int64_min_negate_div",
        "a syntax error (`/-1` after a parenthesis): ANTLR's error recovery takes `/-1` for a \
         `match` path, hidane reports the token (docs/parity-exceptions.md)",
    ),
    (
        "conditions/cel/comparisons/eq_literal/eq_map_double_value",
        "a syntax error (`1e+0`): ANTLR's prediction fails at the `==` before the map, ahead of \
         the token the lexer splits (docs/parity-exceptions.md)",
    ),
    (
        "conditions/cel-self/comparisons/eq_literal/eq_map_double_value",
        "as the case above",
    ),
];

/// The kind of an issue, for the tally: its description up to the first detail.
fn kind(description: &str) -> String {
    for prefix in [
        "Invalid type. Received one of",
        "Invalid type. Permission statements expect a boolean value.",
        "Invalid type. There is no type for variable",
        "The sub-expressions are not comparable",
        "Ternary operator result",
        "Incorrect number of arguments supplied to function",
        "Invalid function name",
        "Invalid variable name",
        "Invalid method name",
        "Invalid service name",
        "Unused variable",
        "Unused function",
        "Ruleset uses old version",
    ] {
        if description.starts_with(prefix) {
            return prefix.to_owned();
        }
    }
    description.to_owned()
}

#[test]
fn warnings_are_the_official_emulators() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/rules_warnings.json"
    );
    let fixture: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let template = fixture["rules"].as_str().unwrap();

    // Per kind of issue: recorded, found by hidane, and given by hidane but not recorded.
    let mut tally: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    // Rulesets that do not compile: all, and those whose message matches in full or in its
    // first line only.
    let (mut errors, mut errors_matched, mut errors_first_line) = (0, 0, 0);
    let mut failures = Vec::new();
    for case in fixture["results"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let content = match case["condition"].as_str() {
            Some(condition) => template.replacen("%s", condition, 1),
            None => case["rules"].as_str().unwrap().to_owned(),
        };
        let compiled = compile(&[SourceFile {
            name: Some("firestore.rules".to_owned()),
            content,
        }]);
        let what = case["condition"]
            .as_str()
            .map_or_else(String::new, |c| format!(": {c}"));
        if let Some(expected) = case["error"].as_str() {
            errors += 1;
            let got = compiled.has_errors().then(|| compiled.error_message());
            let first_line = |m: &str| m.lines().nth(1).map(str::to_owned);
            if got.as_deref() == Some(expected) {
                errors_matched += 1;
                if FIRST_LINE_ONLY.contains(&name) {
                    failures.push(format!("{name}: matches in full now"));
                }
            } else if FIRST_LINE_ONLY.contains(&name)
                && got.as_deref().and_then(first_line) == first_line(expected)
            {
                errors_first_line += 1;
            } else {
                failures.push(format!(
                    "{name}{what}\n  official: {expected:?}\n  hidane:   {got:?}"
                ));
            }
            continue;
        }
        let expected: Vec<Json> = case["issues"].as_array().cloned().unwrap_or_default();
        let got: Vec<Json> = compiled
            .issues
            .iter()
            .map(|i| {
                let p = i.position;
                json!([
                    p.map(|p| p.line),
                    p.map(|p| p.column),
                    p.map(|p| p.current_offset),
                    p.map(|p| p.end_offset),
                    match i.severity {
                        Severity::Warning => "WARNING",
                        Severity::Error => "ERROR",
                    },
                    i.description,
                ])
            })
            .collect();
        let description = |i: &Json| i[5].as_str().unwrap().to_owned();
        let mut unmatched = got.clone();
        for issue in &expected {
            let entry = tally.entry(kind(&description(issue))).or_default();
            entry.0 += 1;
            if let Some(at) = unmatched.iter().position(|g| g == issue) {
                unmatched.remove(at);
                entry.1 += 1;
            }
        }
        for issue in &unmatched {
            tally.entry(kind(&description(issue))).or_default().2 += 1;
        }
        if got != expected {
            let lines = |issues: &[Json]| {
                issues
                    .iter()
                    .map(|i| format!("\n    {i}"))
                    .collect::<String>()
            };
            failures.push(format!(
                "{name}{what}\n  official:{}\n  hidane:{}",
                lines(&expected),
                lines(&got)
            ));
        }
    }
    for (kind, (recorded, found, extra)) in &tally {
        eprintln!("{kind}: {found} of {recorded}, {extra} not recorded");
    }
    eprintln!(
        "compile errors: {errors_matched} of {errors}, {errors_first_line} more in the first line"
    );
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| {
            !KNOWN.iter().any(|(name, _)| {
                f.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with([':', '\n']))
            })
        })
        .collect();
    for f in &unexpected {
        eprintln!("{f}");
    }
    assert!(
        unexpected.is_empty(),
        "{} cases differ from the official emulator",
        unexpected.len()
    );
    assert_eq!(
        failures.len(),
        KNOWN.len(),
        "an allow-listed case matches now"
    );
}
