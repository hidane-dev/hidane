//! `hidane migrate`'s engine: the changes that move a project from the official emulator to
//! hidane, computed from a [`project::scan`] and proposed as edits of each file's text, with
//! the unified diff they make.
//!
//! Only recognised patterns are changed, as little as possible, and a second run changes
//! nothing:
//!
//! - a firebase-tools emulator command the scan read with confidence, in a workflow `run`
//!   step (bash or sh), an npm script or a shell script, gains the prefix `hidane exec -- `;
//! - in a GitHub Actions job that starts the emulators that way, `actions/setup-java` is
//!   replaced by `hidane-dev/setup-hidane@v1` when the job needs Java for nothing else, and
//!   otherwise `hidane-dev/setup-hidane@v1` is added (after setup-java, or before the step that
//!   starts the emulators).
//!
//! Anything else, and anything ambiguous, becomes a note for the user instead of an edit.
//! `FIRESTORE_EMULATOR_HOST` is left as it is: under `hidane exec`, hidane listens where
//! firebase-tools tells it to. Edits are made to the text, so comments and layout stay.

pub mod apply;
pub mod diff;

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha1::{Digest as _, Sha1};

use crate::project::{
    self, Finding, Group, Report,
    ci::{self, Java},
    files::Tree,
    yaml,
};
use diff::TextEdit;

/// The version of the plan's JSON shape.
pub const SCHEMA: &str = "hidane.migrate/1";

/// The action that installs hidane in GitHub Actions.
pub const SETUP_HIDANE: &str = "hidane-dev/setup-hidane@v1";

/// What goes before a firebase-tools command.
pub const PREFIX: &str = "hidane exec -- ";

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub schema: &'static str,
    pub hidane: &'static str,
    /// What the scan found that concerns the change: the emulator commands and the setup-java
    /// steps of the jobs that run them.
    pub detected: Vec<Detected>,
    /// The files to change, by path.
    pub files: Vec<FileEdit>,
    /// What is left to the user: manual changes suggested, and what stays as it is and why.
    pub notes: Vec<Finding>,
    /// Every file's diff, in one patch (`git apply` reads it).
    pub patch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Detected {
    pub file: String,
    pub line: usize,
    pub what: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEdit {
    pub file: String,
    /// The git blob id of the file the plan was made from, so that a copy can be checked to be
    /// that one.
    pub blob: String,
    pub changes: Vec<Change>,
    /// The unified diff.
    pub diff: String,
    /// The file as it is, and as it would be.
    #[serde(skip)]
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Change {
    /// `command.prefix`, `workflow.replace-setup-java`, `workflow.add-setup-hidane`.
    pub code: &'static str,
    pub line: usize,
    pub description: String,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Scans the project and plans the changes.
pub fn plan(tree: &dyn Tree) -> Plan {
    let report = project::scan(tree);
    plan_from(tree, &report)
}

/// The git blob id of a text.
pub fn blob_id(text: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", text.len()).as_bytes());
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Default)]
struct Edits {
    /// Per file: the text edits, and the changes they make.
    by_file: BTreeMap<String, (Vec<TextEdit>, Vec<Change>)>,
    notes: Vec<Finding>,
}

impl Edits {
    /// Adds a change made of `edits`, unless one of them touches text another change edits.
    fn add(&mut self, file: &str, edits: Vec<TextEdit>, change: Change) {
        let (existing, changes) = self.by_file.entry(file.to_owned()).or_default();
        if edits
            .iter()
            .any(|edit| existing.iter().any(|e| diff::overlap(e, edit)))
        {
            self.notes.push(
                Finding::unknown(
                    "migrate.overlap",
                    group_of(file),
                    format!("{file}:{}: two changes would touch the same text; this one is left to you: {}", change.line, change.description),
                )
                .at(file, Some(change.line)),
            );
            return;
        }
        existing.extend(edits);
        changes.push(change);
    }

    fn has_change(&self, file: &str, code: &str) -> bool {
        self.by_file
            .get(file)
            .is_some_and(|(_, changes)| changes.iter().any(|c| c.code == code))
    }
}

fn group_of(file: &str) -> Group {
    if file.starts_with(".github/workflows/") {
        Group::Workflows
    } else {
        Group::Scripts
    }
}

/// Plans the changes from a scan of the same files.
pub fn plan_from(tree: &dyn Tree, report: &Report) -> Plan {
    let facts = &report.facts;
    let commands = &facts.emulator_commands;
    let mut edits = Edits::default();
    let mut detected = Vec::new();

    // Workflow commands of jobs that run on Windows or in another shell stay as they are.
    let mut left = BTreeSet::new();
    for workflow in &facts.workflows {
        for job in workflow.jobs.iter().filter(|j| j.other_shell) {
            left.extend(
                job.emulators
                    .iter()
                    .copied()
                    .filter(|&c| commands[c].file == workflow.file),
            );
        }
    }
    let prefixed = |c: usize| commands[c].needs_prefix() && !left.contains(&c);

    for (i, command) in commands.iter().enumerate() {
        let state = if command.under_hidane {
            "already under hidane exec"
        } else if prefixed(i) {
            "to run under hidane exec"
        } else if !command.starts_firestore() {
            "starts no Firestore emulator; left as it is"
        } else {
            "left as it is: not read with confidence"
        };
        detected.push(Detected {
            file: command.file.clone(),
            line: command.line,
            what: format!("{} ({state})", command.command),
        });
        if prefixed(i)
            && let Some(at) = command.insert_at
        {
            edits.add(
                &command.file,
                vec![TextEdit {
                    start: at,
                    end: at,
                    text: PREFIX.to_owned(),
                }],
                Change {
                    code: "command.prefix",
                    line: command.line,
                    description: format!("run `{}` under hidane exec", short(&command.command)),
                },
            );
        }
    }

    for workflow in &facts.workflows {
        let Some(text) = tree.read(&workflow.file) else {
            continue;
        };
        let doc = yaml::parse(&text);
        for job in &workflow.jobs {
            plan_job(
                &workflow.file,
                &text,
                &doc,
                job,
                &prefixed,
                &mut edits,
                &mut detected,
            );
        }
        let adds_action = edits.has_change(&workflow.file, "workflow.replace-setup-java")
            || edits.has_change(&workflow.file, "workflow.add-setup-hidane");
        if adds_action && pins_by_sha(&doc) {
            edits.notes.push(
                Finding::note(
                    "migrate.pin",
                    Group::Workflows,
                    format!(
                        "{} pins its actions by commit SHA: pin {SETUP_HIDANE} the same way",
                        workflow.file
                    ),
                )
                .at(&workflow.file, None),
            );
        }
    }

    // npm scripts and shell scripts that now run hidane need it installed.
    let hidane_dependency = facts
        .dependencies
        .iter()
        .any(|d| d.ecosystem == "npm" && d.name == "hidane");
    let script_edits: Vec<&String> = edits
        .by_file
        .keys()
        .filter(|f| !f.starts_with(".github/workflows/"))
        .collect();
    if !script_edits.is_empty() && !hidane_dependency {
        edits.notes.push(Finding::note(
            "migrate.install-hidane",
            Group::Scripts,
            format!(
                "{} now {} `hidane exec`: install hidane where they run, for example `npm install --save-dev hidane` (hidane migrate changes no dependency or lockfile)",
                script_edits.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", "),
                if script_edits.len() == 1 { "runs" } else { "run" }
            ),
        ));
    }

    // What the scan could not read.
    for finding in &report.findings {
        if matches!(
            finding.code,
            "manual" | "gcloud.firestore" | "gcloud.datastore-mode" | "workflow.shell"
        ) {
            edits.notes.push(finding.clone());
        }
    }
    // What does not work with hidane yet, whatever the changes.
    let incompatible: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.status == project::Status::Incompatible && !f.code.starts_with("gcloud."))
        .collect();
    if !incompatible.is_empty() {
        edits.notes.push(
            Finding::note(
                "migrate.incompatible",
                Group::Firebase,
                format!(
                    "hidane doctor finds {} incompatible with hidane; `hidane doctor` explains",
                    if incompatible.len() == 1 {
                        "one thing".to_owned()
                    } else {
                        format!("{} things", incompatible.len())
                    }
                ),
            )
            .details(incompatible.iter().map(|f| f.title.clone())),
        );
    }

    let mut files = Vec::new();
    for (file, (text_edits, mut changes)) in edits.by_file {
        let Some(before) = tree.read(&file) else {
            continue;
        };
        changes.sort_by_key(|c| c.line);
        files.push(FileEdit {
            blob: blob_id(&before),
            diff: diff::unified(&file, &before, &text_edits),
            after: diff::apply(&before, &text_edits),
            file,
            changes,
            before,
        });
    }
    detected.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    detected.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.what == b.what);
    let patch = files.iter().map(|f| f.diff.as_str()).collect();
    Plan {
        schema: SCHEMA,
        hidane: env!("CARGO_PKG_VERSION"),
        detected,
        files,
        notes: edits.notes,
        patch,
    }
}

/// A command shortened for a one-line description.
fn short(command: &str) -> String {
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.len() <= 4 {
        command.to_owned()
    } else {
        format!("{} …", words[..4].join(" "))
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_job(
    file: &str,
    text: &str,
    doc: &yaml::Doc,
    job: &ci::Job,
    prefixed: &dyn Fn(usize) -> bool,
    edits: &mut Edits,
    detected: &mut Vec<Detected>,
) {
    if job.emulators.is_empty() {
        return;
    }
    let Some(steps) = doc
        .root
        .as_ref()
        .and_then(|r| r.get("jobs"))
        .and_then(|j| j.get(&job.id))
        .and_then(|j| j.get("steps"))
        .map(yaml::Node::items)
    else {
        return;
    };
    for &s in &job.setup_java {
        if let Some(item) = steps.get(s) {
            detected.push(Detected {
                file: file.to_owned(),
                line: item.line + 1,
                what: format!("actions/setup-java in job `{}`", job.id),
            });
        }
    }
    let needs_hidane = job.emulators.iter().any(|&c| prefixed(c));
    if job.other_shell || !needs_hidane || job.setup_hidane {
        return;
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let java_step = match job.setup_java.as_slice() {
        [one] => steps.get(*one),
        _ => None,
    };
    let problem = java_step.and_then(|step| ci::setup_java_problem(&step.value));
    // A step before the first one that starts the emulators.
    let early = |s: usize| job.first_emulator_step.is_none_or(|f| s < f);
    let replace = matches!(job.java, Java::NotNeeded)
        && problem.is_none()
        && job.setup_java.first().is_some_and(|&s| early(s))
        && java_step.is_some_and(|step| {
            step.value
                .entry("with")
                .is_none_or(|with| with.line != step.line)
        });
    let job_label = format!("{file}, job `{}`", job.id);
    if replace && let Some(step) = java_step {
        let (replacement, description) = replace_setup_java(text, doc, step);
        edits.add(
            file,
            replacement,
            Change {
                code: "workflow.replace-setup-java",
                line: step.line + 1,
                description,
            },
        );
        return;
    }
    // Keep setup-java, and say why.
    if let Some(&last) = job.setup_java.last()
        && let Some(step) = steps.get(last)
    {
        let why = match &job.java {
            Java::Needed(reasons) => {
                edits.notes.push(
                    Finding::info(
                        "migrate.keep-setup-java",
                        Group::Workflows,
                        format!(
                            "{job_label}: actions/setup-java stays: {} still {} Java",
                            reasons.join(", "),
                            if reasons.len() == 1 { "needs" } else { "need" }
                        ),
                    )
                    .at(file, Some(step.line + 1)),
                );
                "which stays".to_owned()
            }
            Java::Unknown(reasons) => {
                edits.notes.push(
                    Finding::unknown(
                        "migrate.keep-setup-java",
                        Group::Workflows,
                        format!("{job_label}: actions/setup-java stays, because whether the job still needs Java is not known — remove it by hand if it does not"),
                    )
                    .at(file, Some(step.line + 1))
                    .details(reasons.iter().cloned()),
                );
                "which stays".to_owned()
            }
            _ => {
                let reason = match (&problem, job.setup_java.len()) {
                    (Some(p), _) => p.reason.clone(),
                    (None, n) if n > 1 => format!("the job has {n} setup-java steps"),
                    _ if !early(last) => {
                        "it comes after the step that starts the emulators".to_owned()
                    }
                    _ => "its `with` inputs start on the step's first line".to_owned(),
                };
                edits.notes.push(
                    Finding::unknown(
                        "migrate.keep-setup-java",
                        Group::Workflows,
                        format!("{job_label}: actions/setup-java is no longer needed, but hidane migrate leaves it ({reason}) — remove it by hand"),
                    )
                    .at(file, Some(step.line + 1)),
                );
                "which hidane migrate leaves".to_owned()
            }
        };
        // After setup-java, unless that is too late.
        if early(last) {
            edits.add(
                file,
                vec![insert_after(text, doc, step, eol)],
                Change {
                    code: "workflow.add-setup-hidane",
                    line: step.line + 1,
                    description: format!("add {SETUP_HIDANE} after actions/setup-java, {why}"),
                },
            );
            return;
        }
    }
    if let Some(first) = job.first_emulator_step
        && let Some(step) = steps.get(first)
    {
        edits.add(
            file,
            vec![insert_before(doc, step, eol)],
            Change {
                code: "workflow.add-setup-hidane",
                line: step.line + 1,
                description: format!(
                    "add {SETUP_HIDANE} before the step that starts the emulators"
                ),
            },
        );
    }
}

/// The edits that turn a setup-java step into a setup-hidane one, and what they do.
fn replace_setup_java(text: &str, doc: &yaml::Doc, step: &yaml::Item) -> (Vec<TextEdit>, String) {
    let mut edits = Vec::new();
    let mut also = Vec::new();
    if let Some(uses) = step.value.entry("uses")
        && let Some(scalar) = uses.value.scalar()
    {
        let line = doc.lines[uses.value.last];
        // A comment naming the old action's version goes with it.
        let end = match scalar.comment {
            Some(c) if version_comment(&text[c..line.end]) => line.end,
            _ => scalar.end,
        };
        edits.push(TextEdit {
            start: scalar.start,
            end,
            text: SETUP_HIDANE.to_owned(),
        });
    }
    if let Some(with) = step.value.entry("with") {
        let last = extend_over_comments(doc, with.last(), with.col);
        edits.push(TextEdit {
            start: doc.lines[with.line].start,
            end: after_line(text, doc.lines[last]),
            text: String::new(),
        });
        also.push("its `with` inputs removed");
    }
    if let Some(name) = step.value.entry("name")
        && let Some(scalar) = name.value.scalar()
    {
        let lower = scalar.text.text.to_ascii_lowercase();
        if ["java", "jdk", "jre", "temurin", "zulu", "corretto"]
            .iter()
            .any(|w| lower.contains(w))
        {
            edits.push(TextEdit {
                start: scalar.start,
                end: scalar.end,
                text: "Set up hidane".to_owned(),
            });
            also.push("the step renamed");
        }
    }
    let also = if also.is_empty() {
        String::new()
    } else {
        format!(" ({})", also.join(", "))
    };
    (
        edits,
        format!(
            "replace actions/setup-java with {SETUP_HIDANE}{also}: only the Firestore emulator needed Java"
        ),
    )
}

/// `# v4`, `# v4.7.1`, `# 4.7.1`: the version a pinned action names.
fn version_comment(comment: &str) -> bool {
    let rest = comment.trim_start_matches('#').trim();
    let rest = rest.strip_prefix(['v', 'V']).unwrap_or(rest);
    !rest.is_empty()
        && rest.starts_with(|c: char| c.is_ascii_digit())
        && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The last of the comment lines after `last` that are indented more than `col`.
fn extend_over_comments(doc: &yaml::Doc, mut last: usize, col: usize) -> usize {
    while let Some(next) = doc.lines.get(last + 1) {
        if next.kind == yaml::LineKind::Comment && next.indent > col {
            last += 1;
        } else {
            break;
        }
    }
    last
}

/// The offset after a line and its line break.
fn after_line(text: &str, line: yaml::Line) -> usize {
    let rest = &text[line.end..];
    if rest.starts_with("\r\n") {
        line.end + 2
    } else if rest.starts_with('\n') {
        line.end + 1
    } else {
        line.end
    }
}

fn setup_hidane_step(indent: usize) -> String {
    format!("{}- uses: {SETUP_HIDANE}", " ".repeat(indent))
}

fn insert_after(text: &str, doc: &yaml::Doc, step: &yaml::Item, eol: &str) -> TextEdit {
    let last = extend_over_comments(doc, step.last(), step.col);
    let line = doc.lines[last];
    let at = after_line(text, line);
    let new_step = setup_hidane_step(step.col);
    let text = if at == line.end {
        format!("{eol}{new_step}")
    } else {
        format!("{new_step}{eol}")
    };
    TextEdit {
        start: at,
        end: at,
        text,
    }
}

fn insert_before(doc: &yaml::Doc, step: &yaml::Item, eol: &str) -> TextEdit {
    // Above the comments that introduce the step.
    let mut first = step.line;
    while first > 0 {
        let prev = doc.lines[first - 1];
        if prev.kind == yaml::LineKind::Comment && prev.indent == step.col {
            first -= 1;
        } else {
            break;
        }
    }
    let at = doc.lines[first].start;
    TextEdit {
        start: at,
        end: at,
        text: format!("{}{eol}", setup_hidane_step(step.col)),
    }
}

/// Whether a workflow pins an action by commit SHA (`uses: owner/repo@<40 hex digits>`).
fn pins_by_sha(doc: &yaml::Doc) -> bool {
    fn walk(node: &yaml::Node) -> bool {
        match &node.kind {
            yaml::NodeKind::Map(entries) => entries.iter().any(|e| {
                (e.key == "uses"
                    && e.value.as_str().is_some_and(|u| {
                        u.rsplit_once('@').is_some_and(|(_, r)| {
                            r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
                        })
                    }))
                    || walk(&e.value)
            }),
            yaml::NodeKind::Seq(items) => items.iter().any(|i| walk(&i.value)),
            _ => false,
        }
    }
    doc.root.as_ref().is_some_and(walk)
}

/// The plan as text: what was detected, the changes, the notes and the diff.
pub fn text(plan: &Plan, path: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "hidane migrate {} — {path}", plan.hidane);
    if !plan.detected.is_empty() {
        let _ = writeln!(out, "\nDetected");
        for d in &plan.detected {
            let _ = writeln!(out, "  {}:{}: {}", d.file, d.line, d.what);
        }
    }
    if plan.files.is_empty() {
        let _ = writeln!(out, "\nNothing to change.");
    } else {
        let _ = writeln!(
            out,
            "\nSuggested changes ({} {})",
            plan.files.len(),
            if plan.files.len() == 1 {
                "file"
            } else {
                "files"
            }
        );
        for file in &plan.files {
            let _ = writeln!(out, "  {}", file.file);
            for change in &file.changes {
                let _ = writeln!(out, "    line {}: {}", change.line, change.description);
            }
        }
    }
    if !plan.notes.is_empty() {
        let _ = writeln!(out, "\nLeft as it is");
        for note in &plan.notes {
            let mark = match (note.status, note.severity) {
                (project::Status::Info, project::Severity::Warning) => "!",
                (status, _) => status.mark(),
            };
            let _ = writeln!(out, "  {mark} {}", note.title);
            for detail in &note.details {
                let _ = writeln!(out, "      {detail}");
            }
        }
    }
    if !plan.patch.is_empty() {
        let _ = writeln!(out);
        out.push_str(&plan.patch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::files::Memory;

    fn memory(files: &[(&str, &str)]) -> Memory {
        files.iter().copied().collect()
    }

    /// The tree with the plan applied.
    fn applied(tree: &Memory, plan: &Plan) -> Memory {
        let mut next = tree.clone();
        for file in &plan.files {
            next.insert(&file.file, file.after.clone());
        }
        next
    }

    const FIREBASE: &str = r#"{"firestore": {}, "emulators": {"firestore": {"port": 8080}}}"#;

    #[test]
    fn replaces_setup_java_and_prefixes_once() {
        let workflow = "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      # Java for the emulator.\n      - name: Set up JDK 21\n        uses: actions/setup-java@v4 # v4.7.1\n        with:\n          distribution: temurin\n          # 21 or newer\n          java-version: '21'\n      - run: |\n          npm ci\n          FOO=1 npx firebase emulators:exec --only firestore \"npm test\" && echo done\n";
        let tree = memory(&[
            ("firebase.json", FIREBASE),
            (".github/workflows/ci.yml", workflow),
        ]);
        let plan = plan(&tree);
        assert_eq!(plan.files.len(), 1, "{plan:#?}");
        assert_eq!(
            plan.files[0].after,
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      # Java for the emulator.\n      - name: Set up hidane\n        uses: hidane-dev/setup-hidane@v1\n      - run: |\n          npm ci\n          FOO=1 hidane exec -- npx firebase emulators:exec --only firestore \"npm test\" && echo done\n"
        );
        let again = super::plan(&applied(&tree, &plan));
        assert!(again.is_empty(), "{:#?}", again.files);
    }

    #[test]
    fn keeps_setup_java_for_storage_and_adds_hidane() {
        let firebase = r#"{"firestore": {}, "storage": {}}"#;
        let workflow = "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n    - uses: actions/setup-java@v4\n      with:\n        java-version: 21\n    - run: firebase emulators:exec 'npm test'";
        let tree = memory(&[
            ("firebase.json", firebase),
            (".github/workflows/ci.yml", workflow),
        ]);
        let plan = plan(&tree);
        assert_eq!(
            plan.files[0].after,
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n    - uses: actions/setup-java@v4\n      with:\n        java-version: 21\n    - uses: hidane-dev/setup-hidane@v1\n    - run: hidane exec -- firebase emulators:exec 'npm test'"
        );
        assert!(
            plan.notes
                .iter()
                .any(|n| n.code == "migrate.keep-setup-java")
        );
        assert!(super::plan(&applied(&tree, &plan)).is_empty());
    }

    #[test]
    fn adds_hidane_before_the_step_without_setup_java() {
        let workflow = "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@0123456789abcdef0123456789abcdef01234567 # v4\n      # The tests.\n      - run: npm run test:emu\n";
        let package =
            r#"{"scripts": {"test:emu": "firebase emulators:exec --only firestore \"npm test\""}}"#;
        let tree = memory(&[
            ("firebase.json", FIREBASE),
            ("package.json", package),
            (".github/workflows/ci.yml", workflow),
        ]);
        let plan = plan(&tree);
        let files: Vec<_> = plan.files.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(files, [".github/workflows/ci.yml", "package.json"]);
        assert!(plan.files[0].after.contains("      - uses: hidane-dev/setup-hidane@v1\n      # The tests.\n      - run: npm run test:emu\n"));
        assert_eq!(
            plan.files[1].after,
            r#"{"scripts": {"test:emu": "hidane exec -- firebase emulators:exec --only firestore \"npm test\""}}"#
        );
        let codes: Vec<_> = plan.notes.iter().map(|n| n.code).collect();
        assert!(codes.contains(&"migrate.pin"), "{codes:?}");
        assert!(codes.contains(&"migrate.install-hidane"), "{codes:?}");
        assert!(super::plan(&applied(&tree, &plan)).is_empty());
    }

    #[test]
    fn crlf_and_order() {
        // setup-java after the step that starts the emulators: hidane goes before that step,
        // with the file's line breaks.
        let workflow = "jobs:\r\n  test:\r\n    runs-on: ubuntu-latest\r\n    steps:\r\n      - run: firebase emulators:exec --only firestore 'npm test'\r\n      - uses: actions/setup-java@v4\r\n";
        let tree = memory(&[
            ("firebase.json", FIREBASE),
            (".github/workflows/ci.yml", workflow),
        ]);
        let plan = plan(&tree);
        assert_eq!(
            plan.files[0].after,
            "jobs:\r\n  test:\r\n    runs-on: ubuntu-latest\r\n    steps:\r\n      - uses: hidane-dev/setup-hidane@v1\r\n      - run: hidane exec -- firebase emulators:exec --only firestore 'npm test'\r\n      - uses: actions/setup-java@v4\r\n"
        );
        assert!(
            plan.notes.iter().any(|n| n
                .title
                .contains("it comes after the step that starts the emulators")),
            "{:#?}",
            plan.notes
        );
        assert!(super::plan(&applied(&tree, &plan)).is_empty());
    }

    #[test]
    fn blob_ids_are_git_s() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            blob_id("hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn version_comments() {
        assert!(version_comment("# v4.7.1"));
        assert!(version_comment("#4"));
        assert!(!version_comment("# Java for the emulator"));
        assert!(!version_comment("# v4 then more"));
    }
}
