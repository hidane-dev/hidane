//! Which documents a `match` covers, against the official emulator (`fixtures/rules_paths.json`,
//! from `tools/oracle/rules_paths.py`): for every rules version and `match` path recorded with
//! `allow read: if true`, a document is readable exactly when some `match` with that `allow`
//! covers it. Paths the official compiler refused are refused here too.

use hidane_rules::{
    SourceFile, Version, compile,
    paths::{Binding, covering},
};
use serde_json::Value as Json;

/// The ruleset `rules_paths.py` loads.
fn rules(version: u64, nested: &[&str], condition: &str) -> String {
    let mut lines = Vec::new();
    if version == 2 {
        lines.push("rules_version = '2';".to_owned());
    }
    lines.push("service cloud.firestore {".to_owned());
    lines.push("  match /databases/{database}/documents {".to_owned());
    let mut indent = "    ".to_owned();
    for path in nested {
        lines.push(format!("{indent}match {path} {{"));
        indent.push_str("  ");
    }
    lines.push(format!("{indent}allow read: if {condition};"));
    for _ in nested {
        indent.truncate(indent.len() - 2);
        lines.push(format!("{indent}}}"));
    }
    lines.push("  }".to_owned());
    lines.push("}".to_owned());
    lines.join("\n") + "\n"
}

fn full_path(document: &str) -> Vec<&str> {
    ["databases", "(default)", "documents"]
        .into_iter()
        .chain(document.split('/'))
        .collect()
}

#[test]
fn matches_cover_the_documents_the_official_emulator_allows() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/rules_paths.json")).unwrap();
    let mut compared = 0;
    for result in fixture["results"].as_array().unwrap() {
        if result["condition"] != "true" {
            continue;
        }
        let version = result["version"].as_u64().unwrap();
        let nested: Vec<&str> = result["match"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        let compiled = compile(&[SourceFile {
            name: Some("firestore.rules".to_owned()),
            content: rules(version, &nested, "true"),
        }]);
        let context = format!("v{version} {nested:?}");
        if result.get("compile").is_some() {
            assert!(compiled.ruleset.is_none(), "{context}: compiles here");
            continue;
        }
        let ruleset = compiled
            .ruleset
            .unwrap_or_else(|| panic!("{context}: does not compile"));
        let service = ruleset.service("cloud.firestore").unwrap();
        for (document, outcome) in result["reads"].as_object().unwrap() {
            let allowed = covering(ruleset.version, service, &full_path(document))
                .iter()
                .any(|c| !c.target().body.allows.is_empty());
            assert_eq!(allowed, outcome == "allow", "{context} {document}");
            compared += 1;
        }
    }
    assert_eq!(compared, 180);
}

fn bindings(version: &str, nested: &[&str], document: &str) -> Vec<Vec<(String, Binding)>> {
    let ruleset = compile(&[SourceFile {
        name: None,
        content: rules(if version == "v2" { 2 } else { 1 }, nested, "true"),
    }])
    .ruleset
    .unwrap();
    let service = ruleset.service("cloud.firestore").unwrap();
    covering(ruleset.version, service, &full_path(document))
        .into_iter()
        .filter(|c| !c.target().body.allows.is_empty())
        .map(|c| c.bindings)
        .collect()
}

fn seg(name: &str, value: &str) -> (String, Binding) {
    (name.to_owned(), Binding::Segment(value.to_owned()))
}

fn path(name: &str, segments: &[&str]) -> (String, Binding) {
    (
        name.to_owned(),
        Binding::Path(segments.iter().map(|s| (*s).to_owned()).collect()),
    )
}

#[test]
fn wildcards_hold_segments_and_recursive_ones_paths() {
    let db = seg("database", "(default)");
    assert_eq!(
        bindings("v1", &["/c/{rest=**}"], "c/d/e/f"),
        [vec![db.clone(), path("rest", &["d", "e", "f"])]]
    );
    assert_eq!(
        bindings("v2", &["/c/d/{rest=**}"], "c/d"),
        [vec![db.clone(), path("rest", &[])]]
    );
    assert!(bindings("v1", &["/c/d/{rest=**}"], "c/d").is_empty());
    assert_eq!(
        bindings("v2", &["/{path=**}/e/{f}"], "a/b/e/f"),
        [vec![db.clone(), path("path", &["a", "b"]), seg("f", "f")]]
    );
    assert_eq!(
        bindings("v2", &["/c/{d}", "/{rest=**}"], "c/d"),
        [vec![db.clone(), seg("d", "d"), path("rest", &[])]]
    );
    // The `match` around the inner one takes part of the path: only the inner one applies.
    assert_eq!(
        bindings("v2", &["/c/{d}", "/e/{f}"], "c/d/e/f"),
        [vec![db, seg("d", "d"), seg("f", "f")]]
    );
}

#[test]
fn versions_parse_as_expected() {
    // `rules` above writes no `rules_version` for version 1.
    let v1 = compile(&[SourceFile {
        name: None,
        content: rules(1, &["/{d=**}"], "true"),
    }]);
    assert_eq!(v1.ruleset.unwrap().version, Version::V1);
}
