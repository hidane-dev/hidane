//! Checks of the files `firebase.json` names: the Security Rules, compiled as hidane loads
//! them, and the functions, for Firestore triggers.

use hidane_rules::{Severity, SourceFile};
use serde::Serialize;

use super::files::{self, Tree};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RulesFile {
    pub file: String,
    pub database: Option<String>,
    pub found: bool,
    pub compiles: bool,
    /// `L<line>:<column> <message>`, as the official emulator words them.
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Compiles a rules file with hidane's compiler, as `--rules` loads it.
pub fn rules(tree: &dyn Tree, file: &str, database: Option<&str>) -> RulesFile {
    let mut result = RulesFile {
        file: file.to_owned(),
        database: database.map(str::to_owned),
        found: false,
        compiles: false,
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    let Some(content) = tree.read(file) else {
        return result;
    };
    result.found = true;
    let compilation = hidane_rules::compile(&[SourceFile {
        name: Some(file.to_owned()),
        content,
    }]);
    for issue in &compilation.issues {
        match issue.severity {
            Severity::Error => result.errors.push(issue.line()),
            Severity::Warning => result.warnings.push(issue.line()),
        }
    }
    result.compiles = compilation.ruleset.is_some() && !compilation.has_errors();
    result
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Functions {
    /// The functions' source directory.
    pub source: String,
    pub found: bool,
    /// Firestore triggers: file, line and the API that declares each.
    pub firestore_triggers: Vec<Trigger>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Trigger {
    pub file: String,
    pub line: usize,
    pub api: String,
}

/// The Firestore trigger APIs of firebase-functions (v2 and v1) and of the Python SDK.
const TRIGGERS: [&str; 8] = [
    "onDocumentCreated",
    "onDocumentUpdated",
    "onDocumentDeleted",
    "onDocumentWritten",
    "on_document_created",
    "on_document_updated",
    "on_document_deleted",
    "on_document_written",
];

const SOURCE_EXTENSIONS: [&str; 7] = [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".py"];

/// Looks for Firestore triggers in the functions at `source`.
pub fn functions(tree: &dyn Tree, source: &str) -> Functions {
    let listing = tree.list(source);
    let mut result = Functions {
        source: source.to_owned(),
        found: !listing.is_empty(),
        firestore_triggers: Vec::new(),
    };
    // TypeScript sources compile to `lib/`: the triggers there are the same ones.
    let compiled = listing
        .iter()
        .any(|e| e.is_dir && e.name == "src")
        .then(|| format!("{source}/lib/"));
    for file in files::walk(tree, source, 6, 2000) {
        if !SOURCE_EXTENSIONS.iter().any(|e| file.ends_with(e))
            || file.ends_with(".d.ts")
            || compiled.as_deref().is_some_and(|lib| file.starts_with(lib))
        {
            continue;
        }
        let Some(text) = tree.read(&file) else {
            continue;
        };
        let mut previous = "";
        for (i, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('#') || code.starts_with('*') {
                continue;
            }
            let api = TRIGGERS
                .iter()
                .find(|api| {
                    line.match_indices(*api).any(|(at, _)| {
                        let rest = line[at + api.len()..].trim_start();
                        // A call or a decorator, not an import; the variants that take the
                        // auth context too (`onDocumentCreatedWithAuthContext`).
                        rest.starts_with('(')
                            || rest.starts_with("WithAuthContext(")
                            || rest.starts_with("_with_auth_context(")
                    })
                })
                .map(|api| (*api).to_owned())
                .or_else(|| {
                    // firebase-functions v1: `functions.firestore.document('…')`, the chain
                    // possibly split over lines.
                    let v1 = line.contains(".document(")
                        && (line.contains("firestore")
                            || previous.trim_end().ends_with("firestore"));
                    v1.then(|| "firestore.document".to_owned())
                });
            if let Some(api) = api {
                result.firestore_triggers.push(Trigger {
                    file: file.clone(),
                    line: i + 1,
                    api,
                });
            }
            if !line.trim().is_empty() {
                previous = line;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::files::Memory;

    #[test]
    fn rules_compile_or_not() {
        let tree: Memory = [
            (
                "firestore.rules",
                "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} {\n      allow read: if true;\n    }\n  }\n}\n",
            ),
            ("bad.rules", "rules_version = '2';\nservice cloud.firestore {\n  match /x {\n    allow read: true;\n  }\n}\n"),
        ]
        .into_iter()
        .collect();
        let good = rules(&tree, "firestore.rules", None);
        assert!(good.found && good.compiles, "{good:?}");
        let bad = rules(&tree, "bad.rules", None);
        assert!(bad.found && !bad.compiles);
        assert!(bad.errors[0].starts_with("L4:"), "{:?}", bad.errors);
        assert!(!rules(&tree, "missing.rules", None).found);
    }

    #[test]
    fn triggers() {
        let tree: Memory = [
            (
                "functions/src/index.ts",
                "import { onDocumentCreated } from 'firebase-functions/v2/firestore';\n// onDocumentWritten( in a comment\nexport const a = onDocumentCreated('users/{id}', () => {});\nexport const b = onDocumentWrittenWithAuthContext('x/{y}', () => {});\nexport const c = functions.firestore\n  .document('c/{d}').onCreate(() => {});\n",
            ),
            ("functions/main.py", "@firestore_fn.on_document_updated(document='a/{b}')\ndef f(event): pass\n"),
            ("functions/node_modules/x/index.js", "onDocumentCreated('a')"),
            // Compiled from src/.
            ("functions/lib/index.js", "exports.a = onDocumentCreated('users/{id}', () => {});"),
        ]
        .into_iter()
        .collect();
        let found = functions(&tree, "functions");
        let got: Vec<_> = found
            .firestore_triggers
            .iter()
            .map(|t| (t.file.as_str(), t.line, t.api.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("functions/main.py", 1, "on_document_updated"),
                ("functions/src/index.ts", 3, "onDocumentCreated"),
                ("functions/src/index.ts", 4, "onDocumentWritten"),
                ("functions/src/index.ts", 6, "firestore.document"),
            ]
        );
    }
}
