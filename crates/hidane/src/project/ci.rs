//! Where a project starts the emulators: the `run` steps of its GitHub Actions workflows, its
//! npm scripts and the shell scripts they run, followed from one to the next. For each
//! workflow job, what else needs Java, so that the scan can tell whether `actions/setup-java`
//! is there only for the Firestore emulator.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    deps::Package,
    files::{self, Tree},
    firebase,
    shell::{self, Kind, ScriptCall},
    text::{Mapped, line_at, line_of},
    yaml,
};

/// A firebase-tools emulator command found in the project.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EmulatorCommand {
    pub file: String,
    pub line: usize,
    /// The command as written (on one line).
    pub command: String,
    /// `emulators:exec` or `emulators:start`.
    pub subcommand: String,
    /// `--only`, split.
    pub only: Option<Vec<String>>,
    /// The emulators it starts, from `--only` and the `firebase.json` it uses; `None` when
    /// that is not known.
    pub starts: Option<Vec<String>>,
    /// The `firebase.json` it uses.
    pub config: Option<String>,
    /// It already runs under `hidane exec`.
    pub under_hidane: bool,
    /// `hidane migrate` can prefix it: the command was read with confidence and is not in a
    /// quoted string.
    pub editable: bool,
    pub import: bool,
    pub export: bool,
    /// The firebase-tools version its launcher names (`npx firebase-tools@15.33.0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firebase_tools: Option<String>,
    /// The byte offset where `hidane exec -- ` goes.
    #[serde(skip)]
    pub insert_at: Option<usize>,
}

impl EmulatorCommand {
    /// It starts the Firestore emulator, or may.
    pub fn starts_firestore(&self) -> bool {
        self.starts
            .as_ref()
            .is_none_or(|s| s.iter().any(|e| e == "firestore"))
    }

    /// `hidane migrate` would prefix it with `hidane exec --`.
    pub fn needs_prefix(&self) -> bool {
        !self.under_hidane && self.editable && self.insert_at.is_some() && self.starts_firestore()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Workflow {
    pub file: String,
    /// The file was read as a workflow.
    pub parsed: bool,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub line: usize,
    pub runs_on: Option<String>,
    /// Steps (0-based indexes) that use `actions/setup-java`.
    pub setup_java: Vec<usize>,
    /// The job installs hidane (`hidane-dev/setup-hidane`, the install script, npm).
    pub setup_hidane: bool,
    /// The emulator commands its steps run, directly or through scripts (indexes into
    /// [`Ci::commands`]).
    pub emulators: Vec<usize>,
    /// The first step that runs an emulator command `hidane migrate` would prefix.
    pub first_emulator_step: Option<usize>,
    /// Whether the job needs Java once hidane serves Firestore.
    pub java: Java,
    /// `FIRESTORE_EMULATOR_HOST` values it sets.
    pub emulator_host: Vec<String>,
    /// It caches `~/.cache/firebase/emulators`.
    pub caches_emulators: bool,
    /// The job runs on Windows, or a step that starts the emulators uses a shell other than
    /// bash or sh.
    pub other_shell: bool,
}

/// Whether a job needs Java besides the Firestore emulator.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "verdict", content = "reasons", rename_all = "kebab-case")]
pub enum Java {
    /// The job starts no emulator.
    NotApplicable,
    /// Only the Firestore emulator needed Java.
    NotNeeded,
    /// These still need it.
    Needed(Vec<String>),
    /// The scan could not tell, for these reasons.
    Unknown(Vec<String>),
}

/// An npm script that starts the emulators.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Script {
    pub file: String,
    pub name: String,
    pub line: usize,
    pub command: String,
    /// The emulator commands in the script itself (indexes into [`Ci::commands`]).
    pub emulators: Vec<usize>,
}

/// Where the emulators are started in a way the scan did not read.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unread {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub reason: String,
}

/// Other things in workflows and scripts worth a finding.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Note {
    pub kind: &'static str,
    pub file: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Ci {
    pub commands: Vec<EmulatorCommand>,
    pub workflows: Vec<Workflow>,
    pub scripts: Vec<Script>,
    pub unread: Vec<Unread>,
    pub notes: Vec<Note>,
    /// firebase-tools versions CI installs (`npm i -g firebase-tools@15`).
    pub firebase_tools: Vec<(String, usize, Option<String>)>,
}

/// A text the commands are read from.
struct Src<'a> {
    file: &'a str,
    /// The file's text, for line numbers.
    file_text: &'a str,
    text: &'a Mapped,
    /// Offsets in `text` map to the file, and the shell is bash or sh.
    editable: bool,
}

/// What one root (a job, a script) reaches.
#[derive(Default)]
struct Reach {
    commands: Vec<usize>,
    /// Commands found in the root's own text.
    direct: Vec<usize>,
    java: Vec<String>,
    unknown: Vec<String>,
    installs_hidane: bool,
    hosts: Vec<String>,
}

impl Reach {
    fn add_command(&mut self, index: usize, depth: usize) {
        if !self.commands.contains(&index) {
            self.commands.push(index);
        }
        if depth == 0 && !self.direct.contains(&index) {
            self.direct.push(index);
        }
    }
}

struct Scanner<'a> {
    tree: &'a dyn Tree,
    configs: &'a [firebase::Config],
    packages: &'a [Package],
    ci: Ci,
    /// Commands by file and offset.
    index: BTreeMap<(String, usize, String), usize>,
    texts: BTreeMap<String, Option<String>>,
}

const MAX_DEPTH: usize = 8;

/// Reads the workflows and the npm scripts.
pub fn scan(
    tree: &dyn Tree,
    configs: &[firebase::Config],
    packages: &[Package],
    workflows: &[String],
) -> Ci {
    let mut scanner = Scanner {
        tree,
        configs,
        packages,
        ci: Ci::default(),
        index: BTreeMap::new(),
        texts: BTreeMap::new(),
    };
    for file in workflows {
        if let Some(text) = tree.read(file) {
            let workflow = scanner.workflow(file, &text);
            scanner.ci.workflows.push(workflow);
        }
    }
    for package in packages {
        for (name, script) in &package.scripts {
            let mut reach = Reach::default();
            let mut visited = BTreeSet::new();
            visited.insert(format!("{}#{name}", package.file));
            let src = Src {
                file: &package.file,
                file_text: &package.text,
                text: script,
                editable: true,
            };
            scanner.walk(
                &src,
                Some(package.dir().to_owned()),
                0,
                &mut reach,
                &mut visited,
            );
            if !reach.direct.is_empty() {
                scanner.ci.scripts.push(Script {
                    file: package.file.clone(),
                    name: name.clone(),
                    line: package.line(script),
                    command: one_line(&script.text),
                    emulators: reach.direct.clone(),
                });
            }
        }
    }
    scanner.ci.unread.sort();
    scanner.ci.unread.dedup();
    scanner.ci.notes.sort();
    scanner.ci.notes.dedup();
    scanner.ci
}

/// Text on one line, at most 160 characters.
fn one_line(text: &str) -> String {
    let unfolded = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let joined = unfolded.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > 160 {
        let cut: String = joined.chars().take(157).collect();
        format!("{cut}…")
    } else {
        joined
    }
}

/// Mentions of the emulators or setup-java in a text the scan could not read as a workflow.
fn mentions(text: &str) -> bool {
    shell::EMULATOR_COMMANDS.iter().any(|c| text.contains(c)) || text.contains("setup-java")
}

impl Scanner<'_> {
    fn text_of(&mut self, file: &str) -> Option<String> {
        self.texts
            .entry(file.to_owned())
            .or_insert_with(|| self.tree.read(file))
            .clone()
    }

    fn unread(&mut self, file: &str, line: usize, text: &str, reason: &str) {
        self.ci.unread.push(Unread {
            file: file.to_owned(),
            line,
            text: one_line(text),
            reason: reason.to_owned(),
        });
    }

    fn workflow(&mut self, file: &str, text: &str) -> Workflow {
        let doc = yaml::parse(text);
        let line_text = |l: usize| -> &str {
            let line = doc.lines[l];
            &text[line.start..line.end]
        };
        let jobs_node = doc.root.as_ref().and_then(|r| r.get("jobs"));
        let Some(jobs_node) = jobs_node.filter(|j| !j.entries().is_empty()) else {
            if mentions(text) {
                let first = text
                    .lines()
                    .position(|l| {
                        shell::EMULATOR_COMMANDS.iter().any(|c| l.contains(c))
                            || l.contains("setup-java")
                    })
                    .unwrap_or(0);
                self.unread(
                    file,
                    first + 1,
                    text.lines().nth(first).unwrap_or(""),
                    "the workflow could not be read",
                );
            }
            return Workflow {
                file: file.to_owned(),
                parsed: false,
                jobs: Vec::new(),
            };
        };
        let root = doc.root.as_ref().expect("jobs come from the root");
        let mut problems_reported = BTreeSet::new();
        for &problem in &doc.problems {
            if mentions(line_text(problem)) && problems_reported.insert(problem) {
                self.unread(
                    file,
                    problem + 1,
                    line_text(problem),
                    "this line could not be read",
                );
            }
        }
        let wf_defaults = root.get("defaults").and_then(|d| d.get("run"));
        let mut hosts_wf = Vec::new();
        env_hosts(root.get("env"), &mut hosts_wf);
        let mut jobs = Vec::new();
        for entry in jobs_node.entries() {
            let job = &entry.value;
            if !matches!(job.kind, yaml::NodeKind::Map(_)) {
                if (job.first..=job.last).any(|l| mentions(line_text(l))) {
                    self.unread(
                        file,
                        entry.line + 1,
                        line_text(entry.line),
                        "the job could not be read",
                    );
                }
                continue;
            }
            if job.get("uses").is_some() {
                // A reusable workflow: its own file is scanned.
                continue;
            }
            let runs_on = job
                .get("runs-on")
                .and_then(yaml::Node::as_str)
                .map(str::to_owned);
            let windows = runs_on
                .as_deref()
                .is_some_and(|r| r.to_ascii_lowercase().contains("windows"));
            let job_defaults = job.get("defaults").and_then(|d| d.get("run"));
            let default = |key: &str| -> Option<String> {
                job_defaults
                    .and_then(|d| d.get(key))
                    .or_else(|| wf_defaults.and_then(|d| d.get(key)))
                    .and_then(yaml::Node::as_str)
                    .map(str::to_owned)
            };
            let mut reach = Reach::default();
            reach.hosts.extend(hosts_wf.iter().cloned());
            env_hosts(job.get("env"), &mut reach.hosts);
            let mut setup_java = Vec::new();
            let mut setup_hidane = false;
            let mut caches = false;
            let mut other_shell = false;
            let mut first_emulator_step = None;
            for (i, item) in job
                .get("steps")
                .map(yaml::Node::items)
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                let step = &item.value;
                if !matches!(step.kind, yaml::NodeKind::Map(_)) {
                    if (item.line..=item.last()).any(|l| mentions(line_text(l))) {
                        self.unread(
                            file,
                            item.line + 1,
                            line_text(item.line),
                            "the step could not be read",
                        );
                        reach.unknown.push(format!(
                            "the step at line {} could not be read",
                            item.line + 1
                        ));
                    }
                    continue;
                }
                env_hosts(step.get("env"), &mut reach.hosts);
                if let Some(uses) = step.get("uses").and_then(yaml::Node::as_str) {
                    let action = uses.split('@').next().unwrap_or(uses).to_ascii_lowercase();
                    match action.as_str() {
                        "actions/setup-java" => setup_java.push(i),
                        "hidane-dev/setup-hidane" => setup_hidane = true,
                        "actions/cache" | "actions/cache/restore" | "actions/cache/save" => {
                            if step
                                .get("with")
                                .and_then(|w| w.get("path"))
                                .and_then(yaml::Node::as_str)
                                .is_some_and(|p| p.contains("firebase/emulators"))
                            {
                                caches = true;
                            }
                        }
                        a if a.starts_with("gradle/")
                            || a.starts_with("android-actions/")
                            || a.contains("setup-maven")
                            || a.contains("setup-gradle") =>
                        {
                            reach.java.push(format!("the action {a}"));
                        }
                        _ => {
                            // An action given an emulator command (a firebase-tools container
                            // action): it runs its own Java.
                            let with_mentions = step.get("with").is_some_and(|w| {
                                (w.first..=w.last).any(|l| {
                                    shell::EMULATOR_COMMANDS
                                        .iter()
                                        .any(|c| line_text(l).contains(c))
                                })
                            });
                            if with_mentions {
                                self.unread(
                                    file,
                                    item.line + 1,
                                    line_text(item.line),
                                    "an action runs the emulators; hidane exec cannot wrap it",
                                );
                                reach.unknown.push(format!("the action {action}"));
                            }
                        }
                    }
                } else if step.get("uses").is_some_and(yaml::Node::is_opaque) {
                    reach.unknown.push(format!(
                        "the step at line {} could not be read",
                        item.line + 1
                    ));
                }
                let Some(run) = step.get("run") else {
                    continue;
                };
                let shell = step
                    .get("shell")
                    .and_then(yaml::Node::as_str)
                    .map(str::to_owned)
                    .or_else(|| default("shell"));
                let posix = match shell.as_deref() {
                    None => !windows,
                    Some(s) => {
                        s == "bash" || s == "sh" || s.starts_with("bash ") || s.starts_with("sh ")
                    }
                };
                let cwd = step
                    .get("working-directory")
                    .and_then(yaml::Node::as_str)
                    .map(str::to_owned)
                    .or_else(|| default("working-directory"));
                let cwd = match cwd {
                    Some(dir) if dir.contains("${{") => None,
                    Some(dir) => files::normalize(&dir),
                    None => Some(String::new()),
                };
                let Some(scalar) = run.scalar() else {
                    if (run.first..=run.last).any(|l| mentions(line_text(l))) {
                        self.unread(
                            file,
                            run.first + 1,
                            line_text(run.first),
                            "the run script could not be read",
                        );
                        reach.unknown.push(format!(
                            "the run script at line {} could not be read",
                            run.first + 1
                        ));
                    }
                    continue;
                };
                let before = reach.commands.len();
                let src = Src {
                    file,
                    file_text: text,
                    text: &scalar.text,
                    editable: posix,
                };
                let mut visited = BTreeSet::new();
                self.walk(&src, cwd, 0, &mut reach, &mut visited);
                let reached = &reach.commands[before..];
                if !reached.is_empty() && !posix {
                    other_shell = true;
                }
                if first_emulator_step.is_none()
                    && reached.iter().any(|&c| self.ci.commands[c].needs_prefix())
                {
                    first_emulator_step = Some(i);
                }
            }
            if windows && !reach.commands.is_empty() {
                other_shell = true;
            }
            let java = self.java(job, &setup_java, &reach, windows);
            reach.hosts.sort();
            reach.hosts.dedup();
            jobs.push(Job {
                id: entry.key.clone(),
                line: entry.line + 1,
                runs_on,
                setup_java,
                setup_hidane: setup_hidane || reach.installs_hidane,
                emulators: reach.commands.clone(),
                first_emulator_step,
                java,
                emulator_host: reach.hosts,
                caches_emulators: caches,
                other_shell,
            });
        }
        Workflow {
            file: file.to_owned(),
            parsed: true,
            jobs,
        }
    }

    /// Whether a job needs Java once hidane serves Firestore.
    fn java(&self, job: &yaml::Node, setup_java: &[usize], reach: &Reach, windows: bool) -> Java {
        if reach.commands.is_empty() {
            return Java::NotApplicable;
        }
        let mut needed = Vec::new();
        let mut unknown = reach.unknown.clone();
        for &c in &reach.commands {
            let command = &self.ci.commands[c];
            match &command.starts {
                Some(starts) => {
                    for e in starts {
                        if e != "firestore" && firebase::JAVA_EMULATORS.contains(&e.as_str()) {
                            let reason = format!("the {e} emulator");
                            if !needed.contains(&reason) {
                                needed.push(reason);
                            }
                        }
                    }
                }
                None => unknown.push(format!(
                    "which emulators `{}` starts is not known",
                    command.command
                )),
            }
        }
        for use_ in &reach.java {
            let reason = if use_.starts_with("the ") {
                use_.clone()
            } else {
                format!("`{use_}`")
            };
            if !needed.contains(&reason) {
                needed.push(reason);
            }
        }
        let steps = job.get("steps").map(yaml::Node::items).unwrap_or_default();
        for &s in setup_java {
            if let Some(step) = steps.get(s)
                && let Some(problem) = setup_java_problem(&step.value)
                && problem.java
            {
                needed.push(problem.reason);
            }
        }
        if windows {
            unknown.push("the job runs on Windows, where hidane exec is not verified".into());
        }
        unknown.dedup();
        if !needed.is_empty() {
            Java::Needed(needed)
        } else if !unknown.is_empty() {
            Java::Unknown(unknown)
        } else {
            Java::NotNeeded
        }
    }

    /// Reads the commands of `src`, run in `cwd`, and follows the scripts they run.
    fn walk(
        &mut self,
        src: &Src,
        mut cwd: Option<String>,
        depth: usize,
        reach: &mut Reach,
        visited: &mut BTreeSet<String>,
    ) {
        let parsed = shell::parse(&src.text.text);
        let mut read = Vec::new();
        for command in &parsed.commands {
            for word in &command.words {
                if let Some(value) = word.text.strip_prefix("FIRESTORE_EMULATOR_HOST=") {
                    reach.hosts.push(value.to_owned());
                }
            }
            let range = (
                command.words.first().map_or(0, |w| w.start),
                command.words.last().map_or(0, |w| w.end),
            );
            let kind = shell::classify(command);
            self.kind(
                src, &kind, false, range, &mut cwd, depth, reach, visited, &mut read,
            );
        }
        for at in shell::unread_mentions(&src.text.text, &parsed, &read) {
            let offset = src.text.offset(at);
            let line = line_of(src.file_text, offset);
            let text = line_at(src.file_text, offset).to_owned();
            self.unread(
                src.file,
                line,
                &text,
                "the emulators are started in a way hidane migrate does not read",
            );
            reach
                .unknown
                .push(format!("`{}` (line {line})", one_line(&text)));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn kind(
        &mut self,
        src: &Src,
        kind: &Kind,
        wrapped: bool,
        range: (usize, usize),
        cwd: &mut Option<String>,
        depth: usize,
        reach: &mut Reach,
        visited: &mut BTreeSet<String>,
        read: &mut Vec<(usize, usize)>,
    ) {
        match kind {
            Kind::Emulators(e) => {
                read.push((e.start, e.end));
                let index = self.command(src, e, cwd.as_deref(), wrapped);
                reach.add_command(index, depth);
                if let Some(script) = &e.script
                    && depth < MAX_DEPTH
                {
                    // The tests emulators:exec runs: they may need Java too.
                    let inner = inline(script, src.text.offset(e.start));
                    let inner_src = Src {
                        file: src.file,
                        file_text: src.file_text,
                        text: &inner,
                        editable: false,
                    };
                    let mut inner_reach = Reach::default();
                    self.walk(
                        &inner_src,
                        cwd.clone(),
                        depth + 1,
                        &mut inner_reach,
                        visited,
                    );
                    reach.java.extend(inner_reach.java);
                    reach.unknown.extend(inner_reach.unknown);
                    for c in inner_reach.commands {
                        reach.add_command(c, depth + 1);
                    }
                }
            }
            Kind::Hidane(inner) => {
                read.push(range);
                self.kind(src, inner, true, range, cwd, depth, reach, visited, read);
            }
            Kind::Script(call) => {
                read.push(range);
                if depth >= MAX_DEPTH {
                    return;
                }
                for (package, name) in self.resolve(call, cwd.as_deref()) {
                    let package = &self.packages[package];
                    let key = format!("{}#{name}", package.file);
                    if !visited.insert(key) {
                        continue;
                    }
                    let Some(script) = package.script(&name) else {
                        continue;
                    };
                    let script = script.clone();
                    let file = package.file.clone();
                    let file_text = package.text.clone();
                    let dir = package.dir().to_owned();
                    let src = Src {
                        file: &file,
                        file_text: &file_text,
                        text: &script,
                        editable: true,
                    };
                    self.walk(&src, Some(dir), depth + 1, reach, visited);
                }
            }
            Kind::ShellFile(path) => {
                read.push(range);
                let Some(file) = cwd.as_deref().and_then(|c| files::join(c, path)) else {
                    reach.unknown.push(format!("the script {path}"));
                    return;
                };
                if depth >= MAX_DEPTH || !visited.insert(file.clone()) {
                    return;
                }
                match self.text_of(&file) {
                    Some(text) => {
                        let mapped = Mapped::verbatim(&text, 0);
                        let src = Src {
                            file: &file,
                            file_text: &text,
                            text: &mapped,
                            editable: true,
                        };
                        self.walk(&src, cwd.clone(), depth + 1, reach, visited);
                    }
                    // A file the job makes itself (`source venv/bin/activate`) is not a
                    // script of the project.
                    None if !path.ends_with(".sh") => {}
                    None => reach.unknown.push(format!("the script {path}, not found")),
                }
            }
            Kind::Inline(script) => {
                read.push(range);
                if depth >= MAX_DEPTH {
                    return;
                }
                let inner = inline(script, src.text.offset(range.0));
                let inner_src = Src {
                    file: src.file,
                    file_text: src.file_text,
                    text: &inner,
                    editable: false,
                };
                self.walk(&inner_src, cwd.clone(), depth + 1, reach, visited);
            }
            Kind::Cd(dir) => {
                *cwd = match (cwd.as_deref(), dir) {
                    (Some(c), Some(d)) if !d.starts_with('/') => files::join(c, d),
                    _ => None,
                };
            }
            Kind::Java(command) => reach.java.push(command.clone()),
            Kind::Gcloud(line) => {
                let offset = src.text.offset(range.0);
                let kind = if line.contains("datastore-mode") || line.contains("datastore ") {
                    "gcloud-datastore"
                } else if line.contains("firestore") {
                    "gcloud-firestore"
                } else {
                    "gcloud"
                };
                self.ci.notes.push(Note {
                    kind,
                    file: src.file.to_owned(),
                    line: line_of(src.file_text, offset),
                    text: one_line(line_at(src.file_text, offset)),
                });
                reach.java.push("`gcloud … emulators`".into());
            }
            Kind::Make => {
                read.push(range);
                let makefile = cwd.as_deref().and_then(|c| files::join(c, "Makefile"));
                if let Some(makefile) = makefile
                    && let Some(text) = self.text_of(&makefile)
                    && mentions(&text)
                {
                    let line = text
                        .lines()
                        .position(|l| shell::EMULATOR_COMMANDS.iter().any(|c| l.contains(c)))
                        .unwrap_or(0);
                    self.unread(
                        &makefile,
                        line + 1,
                        text.lines().nth(line).unwrap_or(""),
                        "a Makefile starts the emulators; hidane migrate does not change Makefiles",
                    );
                    reach
                        .unknown
                        .push(format!("{makefile}, which hidane migrate does not read"));
                }
            }
            Kind::InstallsHidane => reach.installs_hidane = true,
            Kind::InstallsFirebaseTools(version) => {
                let offset = src.text.offset(range.0);
                self.ci.firebase_tools.push((
                    src.file.to_owned(),
                    line_of(src.file_text, offset),
                    version.clone(),
                ));
            }
            Kind::Other => {}
        }
    }

    /// Registers an emulator command, once per place in a file.
    fn command(
        &mut self,
        src: &Src,
        e: &shell::Emulators,
        cwd: Option<&str>,
        wrapped: bool,
    ) -> usize {
        let offset = src.text.offset(e.start);
        let text = one_line(&src.text.text[e.start..e.end.min(src.text.text.len())]);
        let key = (src.file.to_owned(), offset, text.clone());
        if let Some(&index) = self.index.get(&key) {
            return index;
        }
        let config = match (&e.config, cwd) {
            (Some(path), Some(cwd)) => {
                files::join(cwd, path).and_then(|file| self.configs.iter().find(|c| c.file == file))
            }
            (Some(_), None) => None,
            (None, Some(cwd)) => firebase::find(self.configs, cwd),
            (None, None) => firebase::find(self.configs, ""),
        };
        let starts = match config {
            Some(config) if config.doc.is_ok() && !e.only_dynamic => Some(
                config
                    .targets(e.only.as_deref())
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            ),
            // Without a firebase.json, firebase-tools 15.33.0 uses a default configuration when
            // `--only` names neither functions nor hosting (`beforeEmulatorCommand` and
            // `DEFAULT_CONFIG` in src/emulator/commandUtils.ts), and starts what
            // `filterEmulatorTargets` (src/emulator/controller.ts) selects from it; otherwise it
            // stops for want of a configuration, and what would start is not known here.
            None if !e.only_dynamic => e.only.as_ref().and_then(|only| {
                let without_config = only.iter().all(|o| o != "functions" && o != "hosting");
                without_config.then(|| {
                    firebase::default_config()
                        .targets(Some(only))
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                })
            }),
            _ => None,
        };
        let editable = src.editable;
        let command = EmulatorCommand {
            file: src.file.to_owned(),
            line: line_of(src.file_text, offset),
            command: text,
            subcommand: e.subcommand.clone(),
            only: e.only.clone(),
            starts,
            config: config.map(|c| c.file.clone()),
            under_hidane: wrapped,
            editable,
            import: e.import,
            export: e.export,
            firebase_tools: e.version.clone(),
            insert_at: (editable && !wrapped).then_some(offset),
        };
        if !editable && !wrapped {
            self.unread(
                src.file,
                command.line,
                line_at(src.file_text, offset),
                "the command is inside a quoted string or a shell hidane migrate does not read",
            );
        }
        self.ci.commands.push(command);
        let index = self.ci.commands.len() - 1;
        self.index.insert(key, index);
        index
    }

    /// The packages and scripts a script call runs.
    fn resolve(&self, call: &ScriptCall, cwd: Option<&str>) -> Vec<(usize, String)> {
        let mut names = Vec::new();
        for script in &call.scripts {
            if call.tool == "npm" {
                names.push(format!("pre{script}"));
            }
            names.push(script.clone());
            if call.tool == "npm" {
                names.push(format!("post{script}"));
            }
        }
        let having = |name: &str| -> Vec<usize> {
            (0..self.packages.len())
                .filter(|&p| self.packages[p].script(name).is_some())
                .collect()
        };
        let mut found = Vec::new();
        for name in names {
            let packages: Vec<usize> = if call.all {
                having(&name)
            } else if let Some(workspace) = &call.workspace {
                let by_path = cwd.and_then(|c| files::join(c, workspace));
                having(&name)
                    .into_iter()
                    .filter(|&p| {
                        let package = &self.packages[p];
                        package.name.as_deref() == Some(workspace.as_str())
                            || by_path.as_deref() == Some(package.dir())
                    })
                    .collect()
            } else {
                match cwd {
                    Some(cwd) => {
                        let dir = match &call.dir {
                            Some(d) => files::join(cwd, d),
                            None => Some(cwd.to_owned()),
                        };
                        dir.and_then(|d| self.nearest_package(&d))
                            .filter(|&p| self.packages[p].script(&name).is_some())
                            .into_iter()
                            .collect()
                    }
                    None => having(&name),
                }
            };
            found.extend(packages.into_iter().map(|p| (p, name.clone())));
        }
        found
    }

    /// The package whose `package.json` is in `dir` or the nearest directory above it.
    fn nearest_package(&self, dir: &str) -> Option<usize> {
        let mut dir = dir.to_owned();
        loop {
            if let Some(p) = self.packages.iter().position(|p| p.dir() == dir) {
                return Some(p);
            }
            if dir.is_empty() {
                return None;
            }
            dir = files::parent(&dir).to_owned();
        }
    }
}

/// A script in a string (`bash -c '…'`, the tests of `emulators:exec`): every offset maps to
/// where the string is.
fn inline(script: &str, offset: usize) -> Mapped {
    Mapped {
        text: script.to_owned(),
        offsets: vec![offset; script.len() + 1],
    }
}

/// `FIRESTORE_EMULATOR_HOST` in an `env` mapping.
fn env_hosts(env: Option<&yaml::Node>, hosts: &mut Vec<String>) {
    if let Some(value) = env
        .and_then(|e| e.get("FIRESTORE_EMULATOR_HOST"))
        .and_then(yaml::Node::as_str)
    {
        hosts.push(value.to_owned());
    }
}

/// `with` inputs of actions/setup-java that only choose the Java to install.
const SETUP_JAVA_INPUTS: [&str; 6] = [
    "distribution",
    "java-version",
    "java-version-file",
    "java-package",
    "architecture",
    "check-latest",
];

/// Why a setup-java step cannot be replaced as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupJavaProblem {
    pub reason: String,
    /// The reason is itself a use of Java (a build cache, a matrix of Java versions).
    pub java: bool,
}

/// Why a setup-java step cannot be replaced as it is, if it cannot.
pub fn setup_java_problem(step: &yaml::Node) -> Option<SetupJavaProblem> {
    let problem = |reason: String, java: bool| Some(SetupJavaProblem { reason, java });
    for entry in step.entries() {
        match entry.key.as_str() {
            "name" | "uses" | "if" => {
                if entry.value.scalar().is_none() {
                    return problem(format!("its `{}` could not be read", entry.key), false);
                }
            }
            "with" => {
                if !matches!(entry.value.kind, yaml::NodeKind::Map(_)) {
                    return problem("its `with` inputs could not be read".into(), false);
                }
                for input in entry.value.entries() {
                    let key = input.key.as_str();
                    if key == "cache" || key == "cache-dependency-path" {
                        let what = input.value.as_str().unwrap_or("build");
                        return problem(
                            format!("setup-java's `{key}: {what}` input (a Java build cache)"),
                            true,
                        );
                    }
                    if !SETUP_JAVA_INPUTS.contains(&key) {
                        return problem(
                            format!("its input `{key}`, which hidane migrate does not know"),
                            false,
                        );
                    }
                    match input.value.as_str() {
                        Some(v) if v.contains("matrix.") => {
                            return problem(
                                format!("setup-java's `{key}: {v}`: the job runs on several Javas"),
                                true,
                            );
                        }
                        Some(_) => {}
                        None => {
                            return problem(format!("its input `{key}` could not be read"), false);
                        }
                    }
                }
            }
            other => return problem(format!("its `{other}` key"), false),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{deps, files::Memory};

    fn scan_tree(tree: &Memory) -> Ci {
        let files = files::walk(tree, "", 4, 1000);
        let configs: Vec<_> = files
            .iter()
            .filter(|f| files::file_name(f) == "firebase.json")
            .filter_map(|f| firebase::read(tree, f))
            .collect();
        let (_, packages) = deps::scan(tree, &files);
        let workflows: Vec<_> = files
            .iter()
            .filter(|f| f.starts_with(".github/workflows/"))
            .cloned()
            .collect();
        scan(tree, &configs, &packages, &workflows)
    }

    #[test]
    fn a_job_that_only_needs_java_for_firestore() {
        let tree: Memory = [
            ("firebase.json", r#"{"firestore": {}, "emulators": {"firestore": {"port": 8080}, "auth": {}}}"#),
            ("package.json", r#"{"scripts": {"test:emu": "firebase emulators:exec --only firestore,auth 'npm test'", "test": "jest"}}"#),
            (
                ".github/workflows/ci.yml",
                "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      FIRESTORE_EMULATOR_HOST: 127.0.0.1:8080\n    steps:\n      - uses: actions/checkout@v4\n      - uses: actions/setup-java@v4\n        with:\n          distribution: temurin\n          java-version: 21\n      - run: npm ci\n      - run: npm run test:emu\n",
            ),
        ]
        .into_iter()
        .collect();
        let ci = scan_tree(&tree);
        assert_eq!(ci.commands.len(), 1);
        let command = &ci.commands[0];
        assert_eq!(command.file, "package.json");
        assert_eq!(
            command.starts.as_deref(),
            Some(&["auth".to_owned(), "firestore".to_owned()][..])
        );
        assert!(command.needs_prefix());
        let job = &ci.workflows[0].jobs[0];
        assert_eq!(job.setup_java, [1]);
        assert_eq!(job.java, Java::NotNeeded);
        assert_eq!(job.first_emulator_step, Some(3));
        assert_eq!(job.emulator_host, ["127.0.0.1:8080"]);
        assert_eq!(ci.scripts.len(), 1);
        assert!(ci.unread.is_empty(), "{:?}", ci.unread);
    }

    #[test]
    fn other_emulators_and_commands_keep_java() {
        let tree: Memory = [
            ("firebase.json", r#"{"firestore": {}, "storage": {"rules": "s.rules"}}"#),
            (
                ".github/workflows/ci.yml",
                "jobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/setup-java@v4\n        with:\n          java-version: 21\n      - run: npx firebase emulators:exec 'npm test'\n  b:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/setup-java@v4\n        with:\n          java-version: 21\n          cache: gradle\n      - run: |\n          ./gradlew build\n          firebase emulators:exec --only firestore 'npm test'\n  c:\n    runs-on: ubuntu-latest\n    steps:\n      - run: bash -c \"firebase emulators:exec --only firestore 'npm test'\"\n",
            ),
        ]
        .into_iter()
        .collect();
        let ci = scan_tree(&tree);
        let jobs = &ci.workflows[0].jobs;
        assert_eq!(
            jobs[0].java,
            Java::Needed(vec!["the storage emulator".into()])
        );
        assert_eq!(
            jobs[1].java,
            Java::Needed(vec![
                "`gradlew`".into(),
                "setup-java's `cache: gradle` input (a Java build cache)".into()
            ])
        );
        assert!(matches!(jobs[2].java, Java::NotNeeded | Java::Unknown(_)));
        assert_eq!(ci.unread.len(), 1);
        assert!(!ci.commands[2].editable);
    }

    #[test]
    fn without_firebase_json_only_names_the_emulators() {
        let tree: Memory = [(
            ".github/workflows/ci.yml",
            "jobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/setup-java@v4\n        with:\n          java-version: 21\n      - run: |\n          source venv/bin/activate\n          firebase emulators:exec --only database,firestore 'pytest'\n          firebase emulators:exec --only auth 'pytest'\n          firebase emulators:exec --only storage,firestore 'pytest'\n          firebase emulators:exec --only functions,firestore 'pytest'\n",
        )]
        .into_iter()
        .collect();
        let ci = scan_tree(&tree);
        let starts = |i: usize| ci.commands[i].starts.clone();
        assert_eq!(starts(0), Some(vec!["firestore".into(), "database".into()]));
        assert!(!ci.commands[1].starts_firestore());
        // Storage is not in firebase-tools' default configuration.
        assert_eq!(starts(2), Some(vec!["firestore".into()]));
        // With functions, firebase-tools needs a firebase.json.
        assert_eq!(starts(3), None);
        assert_eq!(
            ci.workflows[0].jobs[0].java,
            Java::Needed(vec!["the database emulator".into()])
        );
    }

    #[test]
    fn wrapped_commands_and_mentions() {
        let tree: Memory = [
            ("firebase.json", r#"{"firestore": {}}"#),
            ("package.json", r#"{"scripts": {"emulators:start": "hidane exec -- firebase emulators:start", "dev": "concurrently \"firebase emulators:start\" \"vite\""}}"#),
            (
                ".github/workflows/ci.yml",
                "jobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: hidane-dev/setup-hidane@v1\n      - run: npm run emulators:start &\n",
            ),
        ]
        .into_iter()
        .collect();
        let ci = scan_tree(&tree);
        assert!(ci.commands[0].under_hidane);
        assert!(!ci.commands[0].needs_prefix());
        let job = &ci.workflows[0].jobs[0];
        assert!(job.setup_hidane);
        assert_eq!(job.first_emulator_step, None);
        assert_eq!(ci.unread.len(), 1);
        assert_eq!(ci.unread[0].file, "package.json");
    }
}
