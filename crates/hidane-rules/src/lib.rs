//! Firestore Security Rules as hidane implements them: the lexer, the parser and the checks of
//! the official emulator's compiler (`PUT :securityRules`), with its diagnostics.
//!
//! [`compile`] takes the files of a ruleset and returns the [`Ruleset`] (if it compiled) and
//! the [`Issue`]s: warnings for a ruleset that compiles, warnings and errors for one that does
//! not, in the official emulator's words and positions. The evaluator comes later (#60, #62).
//!
//! What the official emulator does, as measured with `tools/oracle/rules_compile.py`
//! (`crates/hidane/tests/fixtures/rules_compile.json`):
//!
//! - Every file is parsed and its paths checked. Only the first file is checked further and
//!   becomes the ruleset; the others only add their errors.
//! - A syntax error stops there: the message lists the syntax errors and the grammar's own
//!   (`Missing 'if' before conditional expression.`). Otherwise the version, imports and
//!   `match` paths are checked (an unsupported version stops), then namespaces used as
//!   variables and expressions nested too deep, and, if nothing was an error, the names,
//!   functions and types.
//! - Issues are ordered by line, column and text; one about the whole ruleset (the old version
//!   warning) has no position and comes first.

mod ast;
mod check;
mod diag;
pub mod eval;
mod lexer;
mod parser;
pub mod paths;

pub use ast::*;
pub use diag::{Issue, Position, Severity};

/// A file of a ruleset, as `PUT :securityRules` sends it.
#[derive(Debug, Clone, Default)]
pub struct SourceFile {
    pub name: Option<String>,
    pub content: String,
}

/// What [`compile`] found.
#[derive(Debug, Clone)]
pub struct Compilation {
    /// The ruleset, when there is no error.
    pub ruleset: Option<Ruleset>,
    pub issues: Vec<Issue>,
}

impl Compilation {
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// The official emulator's message for a ruleset that does not compile: every issue, one
    /// per line.
    pub fn error_message(&self) -> String {
        let mut message = "Error compiling rules:".to_owned();
        for issue in &self.issues {
            message.push('\n');
            message.push_str(&issue.line());
        }
        message
    }
}

/// Compiles the files of a ruleset. Without files there is nothing to compile: the result is
/// an error about empty content.
///
/// Parsing and checking recurse as deep as the input nests, so they run on a thread of their
/// own with a large stack; the ruleset that comes back is at most a few thousand levels deep.
pub fn compile(files: &[SourceFile]) -> Compilation {
    std::thread::scope(|scope| {
        let thread = std::thread::Builder::new()
            .name("hidane-rules".into())
            .stack_size(COMPILE_STACK)
            .spawn_scoped(scope, || compile_here(files));
        match thread {
            Ok(thread) => thread
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            // Without a thread, on this one: the parser's limits still bound the depth.
            Err(_) => compile_here(files),
        }
    })
}

/// The stack of the compiling thread. Only the part used is committed.
const COMPILE_STACK: usize = 256 * 1024 * 1024;

fn compile_here(files: &[SourceFile]) -> Compilation {
    if files.is_empty() || files.iter().any(|f| f.content.is_empty()) {
        return Compilation {
            ruleset: None,
            issues: vec![Issue {
                file_name: None,
                position: None,
                description: "Rules content empty. Compilation aborted.".to_owned(),
                severity: Severity::Error,
            }],
        };
    }
    let mut parsed: Vec<parser::Parsed> = files.iter().map(|f| parser::parse(&f.content)).collect();
    let finish = |parsed: Vec<parser::Parsed>, ruleset| {
        let mut issues = Vec::new();
        for (file, p) in files.iter().zip(parsed) {
            let mut diags = p.diags;
            diag::sort(&mut diags);
            issues.extend(
                diags
                    .into_iter()
                    .map(|d| d.into_issue(&file.content, file.name.as_deref())),
            );
        }
        Compilation { ruleset, issues }
    };
    if parsed.iter().any(|p| p.syntax_error) {
        return finish(parsed, None);
    }
    let mut supported = true;
    let mut versions = Vec::new();
    for p in &mut parsed {
        let version = check::version(&p.file, &mut p.diags);
        let too_deep = check::structure(&p.file, version, &mut p.diags);
        supported &= version.is_some() && !too_deep;
        versions.push(version);
    }
    if !supported {
        return finish(parsed, None);
    }
    for p in &mut parsed {
        check::packages(&p.file, &mut p.diags);
        check::complexity(&p.file, &mut p.diags);
    }
    let has_errors = |parsed: &[parser::Parsed]| {
        parsed
            .iter()
            .flat_map(|p| &p.diags)
            .any(|d| d.severity == Severity::Error)
    };
    if has_errors(&parsed) {
        return finish(parsed, None);
    }
    let version = versions[0].unwrap_or(Version::V1);
    let first = &mut parsed[0];
    check::semantics(&first.file, version, &mut first.diags);
    if has_errors(&parsed) {
        return finish(parsed, None);
    }
    let first = std::mem::take(&mut parsed[0].file);
    let ruleset = Ruleset {
        version,
        file_name: files[0].name.clone(),
        functions: first.functions,
        services: first.services,
    };
    finish(parsed, Some(ruleset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, content: &str) -> SourceFile {
        SourceFile {
            name: Some(name.to_owned()),
            content: content.to_owned(),
        }
    }

    fn rules(condition: &str) -> String {
        format!(
            "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n    match /a/{{x}} {{\n      allow read: if {condition};\n    }}\n  }}\n}}\n"
        )
    }

    #[test]
    fn a_ruleset_with_warnings() {
        let c = compile(&[file("f.rules", &rules("'😀' != '' && foo() && x != null"))]);
        assert!(!c.has_errors());
        let ruleset = c.ruleset.expect("compiled");
        assert_eq!(ruleset.version, Version::V2);
        assert!(ruleset.service("cloud.firestore").is_some());
        let issues: Vec<_> = c
            .issues
            .iter()
            .map(|i| {
                let p = i.position.unwrap();
                (
                    p.line,
                    p.column,
                    p.current_offset,
                    p.end_offset,
                    i.description.as_str(),
                )
            })
            .collect();
        // Columns and offsets count code points, and the end offset is inclusive.
        assert_eq!(
            issues,
            vec![
                (5, 35, 142, 144, "Invalid function name: foo."),
                (
                    5,
                    46,
                    153,
                    154,
                    "The sub-expressions are not comparable, so this comparison will always return true."
                ),
            ]
        );
        assert_eq!(c.issues[0].file_name.as_deref(), Some("f.rules"));
    }

    #[test]
    fn errors_come_with_the_warnings_found_with_them() {
        let src = "service cloud.firestore {\n  function f() { let a = 1; return a == 1; }\n  match /a { allow read: if f(); }\n}\n";
        let c = compile(&[file("f.rules", src)]);
        assert!(c.has_errors() && c.ruleset.is_none());
        assert_eq!(
            c.error_message(),
            "Error compiling rules:\n\
             L0:0 Ruleset uses old version (version [1]). Please update to the latest version (version [2]).\n\
             L2:22 Let bindings are not allowed in rules_version=\"1\";"
        );
    }

    #[test]
    fn only_the_first_file_is_checked_in_full() {
        let c = compile(&[
            file("a.rules", &rules("true")),
            file("b.rules", &rules("foo()")),
        ]);
        assert!(c.issues.is_empty() && c.ruleset.is_some());
        let c = compile(&[
            file("a.rules", &rules("true")),
            file("b.rules", &rules("true &&")),
        ]);
        assert!(c.has_errors());
        assert_eq!(c.issues[0].file_name.as_deref(), Some("b.rules"));
        let c = compile(&[file("a.rules", &rules("true")), file("b.rules", "")]);
        assert_eq!(
            c.error_message(),
            "Error compiling rules:\nL0:0 Rules content empty. Compilation aborted."
        );
    }

    #[test]
    fn stages() {
        // A syntax error hides the path checks; without one, they come with the grammar's.
        let bad_path = |condition: &str| {
            format!(
                "rules_version = '2';\nservice s {{\n  match /{{a}}/{{a}} {{\n    allow read: {condition};\n  }}\n}}\n"
            )
        };
        let c = compile(&[file("f", &bad_path("if true &&"))]);
        assert_eq!(c.issues.len(), 1);
        let c = compile(&[file("f", &bad_path("true"))]);
        let lines: Vec<_> = c.issues.iter().map(Issue::line).collect();
        assert_eq!(
            lines,
            vec![
                "L3:15 Variable a already exists in the PathTemplate.",
                "L4:17 Missing 'if' before conditional expression.",
            ]
        );
        // An unsupported version stops before the namespaces are checked.
        let c = compile(&[file(
            "f",
            "rules_version = '3';\nservice s { match /a { allow read: if math == 1; } }",
        )]);
        assert_eq!(
            c.error_message(),
            "Error compiling rules:\nL1:1 Unsupported rules version 3."
        );
    }

    /// Input nested far beyond the limits is refused, not a stack overflow.
    #[test]
    fn deep_input() {
        let service = |body: String| format!("rules_version = '2';\nservice s {{\n{body}}}\n");
        for (src, first) in [
            (
                rules(&format!("{}true{}", "(".repeat(50_000), ")".repeat(50_000))),
                "L5:121 Expression is too complex to evaluate safely.",
            ),
            (
                rules(&format!("{}true", "!".repeat(50_000))),
                "L5:121 Expression is too complex to evaluate safely.",
            ),
            (
                rules(&vec!["true"; 200_000].join(" && ")),
                "L5:22 Expression is too complex to evaluate safely.",
            ),
            (
                rules(&format!("request{} == 1", ".a".repeat(100_000))),
                "L5:4029 Expression is too complex to evaluate safely.",
            ),
            (
                service(format!(
                    "{}allow read;\n{}",
                    "match /a {\n".repeat(5_000),
                    "}\n".repeat(5_000)
                )),
                "L66:1 Rule hierarchy is too complex to evaluate safely.",
            ),
        ] {
            let c = compile(&[file("f", &src)]);
            assert!(c.has_errors());
            assert_eq!(c.issues[0].line(), first);
        }
    }
}
