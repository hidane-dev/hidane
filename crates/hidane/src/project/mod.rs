//! `hidane doctor`'s engine: reads a Firebase project (without running anything, without the
//! network) and says, for each part of it, whether it works with hidane.
//!
//! [`scan`] takes the project's files through a [`files::Tree`] (a directory, or files held in
//! memory) and returns a [`Report`]: the facts it read (`firebase.json`, the rules, the SDK
//! versions, the workflows and scripts that start the emulators) and its findings, each with a
//! [`Status`]. A finding is `compatible` only when something hidane verified backs it, and
//! names that in its `source`; what nothing backs is `unknown`, and a known gap is
//! `incompatible`. The report serialises to JSON (`hidane doctor --format json`), whose
//! `schema` field versions its shape.

pub mod checks;
pub mod ci;
pub mod deps;
pub mod files;
pub mod findings;
pub mod firebase;
pub mod json;
pub mod report;
pub mod shell;
pub mod text;
pub mod verified;
pub mod yaml;

use serde::Serialize;

pub use findings::{Finding, Group, Severity, Status, Summary};

/// The version of the report's JSON shape.
pub const SCHEMA: &str = "hidane.doctor/1";

/// What a scan read.
#[derive(Debug, Clone, Serialize)]
pub struct Facts {
    pub firebase: Vec<firebase::Facts>,
    pub rules: Vec<checks::RulesFile>,
    pub functions: Vec<checks::Functions>,
    pub dependencies: Vec<deps::Dependency>,
    /// Every firebase-tools emulator command found.
    pub emulator_commands: Vec<ci::EmulatorCommand>,
    pub workflows: Vec<ci::Workflow>,
    pub scripts: Vec<ci::Script>,
    /// Where the emulators are started in a way the scan did not read.
    pub unread: Vec<ci::Unread>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema: &'static str,
    /// The hidane version that wrote the report.
    pub hidane: &'static str,
    pub summary: Summary,
    pub findings: Vec<Finding>,
    pub facts: Facts,
}

impl Report {
    /// Whether a finding is `incompatible`.
    pub fn has_incompatibility(&self) -> bool {
        self.summary.incompatible > 0
    }
}

/// How deep the scan looks for manifests and `firebase.json`.
const DEPTH: usize = 4;
const MAX_FILES: usize = 20_000;

/// Reads the project and checks it.
pub fn scan(tree: &dyn files::Tree) -> Report {
    let all = files::walk(tree, "", DEPTH, MAX_FILES);
    let configs: Vec<firebase::Config> = all
        .iter()
        .filter(|f| files::file_name(f) == "firebase.json")
        .filter_map(|f| firebase::read(tree, f))
        .collect();
    let firebase: Vec<firebase::Facts> =
        configs.iter().filter_map(firebase::Config::facts).collect();
    let mut rules = Vec::new();
    let mut functions = Vec::new();
    for (config, facts) in configs.iter().filter_map(|c| Some((c, c.facts()?))) {
        let dir = config.dir();
        if let Some(firestore) = &facts.firestore {
            for database in &firestore.databases {
                if let Some(path) = database.rules.as_deref().and_then(|r| files::join(dir, r)) {
                    rules.push(checks::rules(tree, &path, database.database.as_deref()));
                }
            }
        }
        if config.starts("functions", None) {
            for source in &facts.functions {
                if let Some(path) = files::join(dir, source) {
                    functions.push(checks::functions(tree, &path));
                }
            }
        }
    }
    let (dependencies, packages) = deps::scan(tree, &all);
    let workflows: Vec<String> = tree
        .list(".github/workflows")
        .into_iter()
        .filter(|e| !e.is_dir && (e.name.ends_with(".yml") || e.name.ends_with(".yaml")))
        .map(|e| format!(".github/workflows/{}", e.name))
        .collect();
    let ci = ci::scan(tree, &configs, &packages, &workflows);
    let facts = Facts {
        firebase,
        rules,
        functions,
        dependencies,
        emulator_commands: ci.commands.clone(),
        workflows: ci.workflows.clone(),
        scripts: ci.scripts.clone(),
        unread: ci.unread.clone(),
    };
    let mut findings = Vec::new();
    firebase_findings(&configs, &facts, &mut findings);
    rules_findings(&facts, &mut findings);
    functions_findings(&facts, &mut findings);
    let tools = dependency_findings(&facts, &ci, &mut findings);
    ui_findings(&configs, &facts, &tools, &mut findings);
    workflow_findings(&facts, &ci, &mut findings);
    script_findings(&facts, &mut findings);
    findings.sort_by_key(|f| f.group);
    Report {
        schema: SCHEMA,
        hidane: env!("CARGO_PKG_VERSION"),
        summary: Summary::of(&findings),
        findings,
        facts,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

fn firebase_findings(configs: &[firebase::Config], facts: &Facts, out: &mut Vec<Finding>) {
    let g = Group::Firebase;
    if configs.is_empty() {
        out.push(
            Finding::unknown(
                "firebase-json.missing",
                g,
                "No firebase.json found: nothing here starts the emulators through firebase-tools",
            )
            .detail("hidane also runs on its own: `hidane --host 127.0.0.1 --port 8080`, with FIRESTORE_EMULATOR_HOST=127.0.0.1:8080"),
        );
        return;
    }
    for config in configs {
        let file = config.file.as_str();
        if let Err(err) = &config.doc {
            out.push(
                Finding::unknown(
                    "firebase-json.invalid",
                    g,
                    format!("{file} could not be read: {err}"),
                )
                .severity(Severity::Error)
                .at(file, None),
            );
            continue;
        }
        let Some(f) = facts.firebase.iter().find(|f| f.file == file) else {
            out.push(
                Finding::unknown(
                    "firebase-json.invalid",
                    g,
                    format!("{file} is not a JSON object"),
                )
                .at(file, None),
            );
            continue;
        };
        out.push(Finding::ok("firebase-json.found", g, format!("{file} found")).at(file, None));
        match &f.firestore {
            Some(fs) => {
                let mut finding = Finding::ok(
                    "firestore-emulator.configured",
                    g,
                    format!("Firestore emulator configured: {}:{}", fs.host, fs.port),
                )
                .at(file, None)
                .detail("Under `hidane exec`, hidane listens where firebase-tools tells it to: FIRESTORE_EMULATOR_HOST stays as it is");
                if let Some(ws) = fs.websocket_port {
                    finding = finding.detail(format!(
                        "websocketPort {ws}: hidane serves the Emulator UI's request monitor there"
                    ));
                }
                out.push(finding);
                match fs.edition.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("enterprise") => out.push(
                        Finding::compatible(
                            "firestore.enterprise-edition",
                            g,
                            "Enterprise edition: hidane answers queries, listeners, pipelines and writes as an enterprise database does",
                            verified::ENTERPRISE,
                        )
                        .at(file, None),
                    ),
                    Some("standard") | None => {}
                    Some(other) => out.push(
                        Finding::note(
                            "firestore.edition",
                            g,
                            format!("Unknown Firestore edition `{other}`: firebase-tools accepts only standard and enterprise"),
                        )
                        .at(file, None),
                    ),
                }
                if fs.databases.len() > 1 {
                    out.push(
                        Finding::info(
                            "firestore.databases",
                            g,
                            format!(
                                "{} Firestore databases configured: firebase-tools then loads no rules into the emulator, whichever emulator it is",
                                fs.databases.len()
                            ),
                        )
                        .at(file, None),
                    );
                }
                let java: Vec<&str> = f
                    .emulators
                    .iter()
                    .filter(|e| e.java)
                    .map(|e| e.name.as_str())
                    .collect();
                if java.is_empty() {
                    out.push(
                        Finding::ok(
                            "java.firestore-only",
                            g,
                            "Only the Firestore emulator needs Java here: under `hidane exec`, firebase-tools needs no Java",
                        )
                        .at(file, None),
                    );
                } else {
                    out.push(
                        Finding::note(
                            "java.other-emulators",
                            g,
                            format!(
                                "The {} {} still {} Java: hidane serves only Firestore, and `hidane exec` runs the others with the Java on PATH",
                                java.join(", "),
                                if java.len() == 1 { "emulator" } else { "emulators" },
                                if java.len() == 1 { "needs" } else { "need" },
                            ),
                        )
                        .at(file, None),
                    );
                }
            }
            None => out.push(
                Finding::note(
                    "firestore-emulator.missing",
                    g,
                    format!("{file} configures no Firestore emulator: `firebase emulators:start` does not start one, so hidane has nothing to stand in for there"),
                )
                .at(file, None),
            ),
        }
        let others: Vec<&firebase::Emulator> = f.emulators.iter().collect();
        if !others.is_empty() {
            let names: Vec<&str> = others.iter().map(|e| e.name.as_str()).collect();
            out.push(
                Finding::info(
                    "emulators.others",
                    g,
                    format!("Other emulators: {}", names.join(", ")),
                )
                .at(file, None)
                .details(others.iter().map(|e| {
                    let port = e.port.map(|p| format!(" (port {p})")).unwrap_or_default();
                    if e.java {
                        format!(
                            "{}{port}: runs on Java, which hidane does not replace",
                            e.name
                        )
                    } else {
                        format!(
                            "{}{port}: runs without Java; hidane does not change it",
                            e.name
                        )
                    }
                })),
            );
        }
    }
}

/// The Emulator UI, whose version is the one the firebase-tools in use pins: verified as far
/// as that firebase-tools is (`tools`, the status of each firebase-tools version found).
fn ui_findings(
    configs: &[firebase::Config],
    facts: &Facts,
    tools: &[Status],
    out: &mut Vec<Finding>,
) {
    let g = Group::Firebase;
    // The least verified of the versions found.
    let least = tools.iter().copied().max_by_key(|s| match s {
        Status::Compatible => 0,
        Status::LikelyCompatible => 1,
        _ => 2,
    });
    let (ui, source) = verified::EMULATOR_UI;
    for config in configs {
        let Some(f) = facts.firebase.iter().find(|f| f.file == config.file) else {
            continue;
        };
        if !(f.ui && f.firestore.is_some() && config_has(config, &["emulators", "ui"])) {
            continue;
        }
        let finding = match least {
            Some(Status::Compatible) => Finding::compatible(
                "emulators.ui",
                g,
                format!("Emulator UI: verified with hidane ({ui})"),
                source,
            ),
            Some(Status::LikelyCompatible) => Finding::likely_compatible(
                "emulators.ui",
                g,
                format!(
                    "Emulator UI: likely compatible — hidane was verified with {}; the firebase-tools here may download another version",
                    verified::EMULATOR_UI_VERSION
                ),
                source,
            ),
            _ => Finding::unknown(
                "emulators.ui",
                g,
                format!(
                    "Emulator UI: its version follows firebase-tools, whose version is not verified here — manual verification required (hidane was verified with {})",
                    verified::EMULATOR_UI_VERSION
                ),
            ),
        };
        out.push(finding.at(&config.file, None));
    }
}

fn config_has(config: &firebase::Config, path: &[&str]) -> bool {
    config.doc.as_ref().is_ok_and(|d| d.at(path).is_some())
}

fn rules_findings(facts: &Facts, out: &mut Vec<Finding>) {
    let g = Group::Rules;
    for rules in &facts.rules {
        let file = rules.file.as_str();
        if !rules.found {
            out.push(
                Finding::note(
                    "rules.missing",
                    g,
                    format!("{file} not found: firebase-tools starts the emulator without rules (every request allowed), whichever emulator it is"),
                )
                .at(file, None),
            );
        } else if rules.compiles {
            let warnings = rules.warnings.len();
            let title = if warnings == 0 {
                format!("{file} compiles with hidane's rules compiler")
            } else {
                format!(
                    "{file} compiles with hidane's rules compiler, with {}",
                    plural(warnings, "warning", "warnings")
                )
            };
            out.push(
                Finding::compatible("rules.compile", g, title, verified::RULES_COMPILER)
                    .at(file, None)
                    .details(rules.warnings.iter().map(|w| format!("warning: {w}"))),
            );
        } else {
            out.push(
                Finding::incompatible(
                    "rules.compile",
                    g,
                    format!("{file} does not compile: hidane would start without these rules"),
                    verified::RULES_COMPILER,
                )
                .at(file, None)
                .details(rules.errors.iter().map(|e| format!("error: {e}")))
                .details(rules.warnings.iter().map(|w| format!("warning: {w}")))
                .detail("hidane's compiler gives the official emulator's errors; if the official emulator accepts this file, please report it (https://github.com/hidane-dev/hidane/issues)"),
            );
        }
    }
    for f in &facts.firebase {
        let Some(fs) = &f.firestore else {
            continue;
        };
        if fs.databases.iter().all(|d| d.rules.is_none()) {
            out.push(
                Finding::info(
                    "rules.none",
                    g,
                    "No Firestore rules file in firebase.json: both emulators allow every request",
                )
                .at(&f.file, None),
            );
        }
        for indexes in fs.databases.iter().filter_map(|d| d.indexes.as_deref()) {
            out.push(
                Finding::compatible(
                    "rules.indexes",
                    g,
                    format!("{indexes}: neither emulator enforces composite indexes"),
                    verified::INDEXES,
                )
                .at(&f.file, None),
            );
        }
    }
}

fn functions_findings(facts: &Facts, out: &mut Vec<Finding>) {
    let g = Group::Functions;
    for functions in &facts.functions {
        let source = functions.source.as_str();
        if !functions.found {
            continue;
        }
        let triggers = &functions.firestore_triggers;
        if triggers.is_empty() {
            out.push(Finding::info(
                "functions.no-firestore-triggers",
                g,
                format!("Functions in {source}: no Firestore triggers found; hidane does not change the Functions emulator"),
            ));
            continue;
        }
        let (python, node): (Vec<&checks::Trigger>, Vec<&checks::Trigger>) = triggers
            .iter()
            .partition(|t| t.api.starts_with("on_document_"));
        if !node.is_empty() {
            // Judged by the firebase-functions the functions depend on.
            let dependency = facts.dependencies.iter().find(|d| {
                d.ecosystem == "npm"
                    && d.name == "firebase-functions"
                    && files::parent(&d.file) == source
            });
            let what = plural(node.len(), "Firestore trigger", "Firestore triggers");
            let shown = dependency
                .and_then(|d| d.resolved.as_deref().or(d.declared.as_deref()))
                .unwrap_or("of no known version");
            let verdict = judge(
                "functions.firestore-triggers",
                "npm",
                "firebase-functions",
                shown,
                dependency.and_then(concrete),
                dependency.and_then(|d| d.declared.as_deref()),
            );
            let title = match verdict.status {
                Status::Compatible => format!(
                    "{what} in {source} (firebase-functions {shown}): their events reach the Functions emulator as with the official emulator"
                ),
                Status::LikelyCompatible => format!(
                    "{what} in {source} (firebase-functions {shown}): likely compatible — the same major version as the verified {}; this version itself is not verified",
                    verified::verified("npm", "firebase-functions").map_or("", |v| v.version)
                ),
                _ => format!(
                    "{what} in {source} (firebase-functions {shown}): not verified with hidane — manual verification required"
                ),
            };
            let mut finding = verdict;
            finding.group = g;
            finding.title = title;
            out.push(finding.details(trigger_lines(&node)));
        }
        if !python.is_empty() {
            out.push(
                Finding::unknown(
                    "functions.firestore-triggers",
                    g,
                    format!(
                        "{} in {source} (Python): not verified with hidane — manual verification required",
                        plural(python.len(), "Firestore trigger", "Firestore triggers")
                    ),
                )
                .detail("hidane verified the events of firebase-functions (Node) triggers")
                .details(trigger_lines(&python)),
            );
        }
    }
}

/// The first triggers, one per line.
fn trigger_lines(triggers: &[&checks::Trigger]) -> Vec<String> {
    let mut lines: Vec<String> = triggers
        .iter()
        .take(5)
        .map(|t| format!("{}:{}: {}", t.file, t.line, t.api))
        .collect();
    if triggers.len() > 5 {
        lines.push(format!("and {} more", triggers.len() - 5));
    }
    lines
}

/// How a package is called in findings.
fn label(ecosystem: &str, name: &str) -> String {
    match (ecosystem, name) {
        ("npm", "firebase") => "Firebase JS SDK (firebase)".into(),
        ("npm", "firebase-admin") => "Firebase Admin SDK (firebase-admin)".into(),
        ("pypi", "firebase-admin") => "Firebase Admin SDK for Python (firebase-admin)".into(),
        ("pypi", "google-cloud-firestore") => "google-cloud-firestore (Python)".into(),
        ("go", "firebase.google.com/go/v4") => "Firebase Admin SDK for Go".into(),
        ("maven", "com.google.firebase:firebase-admin") => "Firebase Admin SDK for Java".into(),
        ("maven", "com.google.cloud:google-cloud-firestore") => {
            "google-cloud-firestore (Java)".into()
        }
        ("maven", "com.google.firebase:firebase-firestore") => {
            "Firebase Android SDK (firebase-firestore)".into()
        }
        ("pub", "cloud_firestore") => "FlutterFire cloud_firestore".into(),
        (_, name) => name.into(),
    }
}

/// A verdict on a package hidane may have verified. `concrete` is the version it is known to
/// be (resolved by a lockfile, or pinned exactly), `range` what is declared.
///
/// - `compatible`: the concrete version has the verified version's major and minor versions;
/// - `likely-compatible`: only the major version is the same (or a range pins only it);
/// - `unknown`: anything else.
fn judge(
    code: &'static str,
    ecosystem: &str,
    name: &str,
    shown: &str,
    concrete: Option<&str>,
    range: Option<&str>,
) -> Finding {
    let g = Group::Dependencies;
    let what = label(ecosystem, name);
    let Some(v) = verified::verified(ecosystem, name) else {
        return Finding::unknown(
            code,
            g,
            format!("{what} {shown}: not verified with hidane — manual verification required"),
        );
    };
    let not_verified = || {
        Finding::unknown(
            code,
            g,
            format!("{what} {shown}: not verified with hidane — manual verification required"),
        )
        .detail(format!("hidane verified {name} {}", v.version))
    };
    if let Some(c) = concrete {
        let minor = deps::major_minor(c);
        if minor.is_some() && minor == deps::major_minor(v.version) {
            let how = if c.trim_start_matches(['v', 'V']) == v.version {
                format!("verified with hidane ({})", v.what)
            } else {
                format!(
                    "verified with hidane at {} ({}); only the patch version differs",
                    v.version, v.what
                )
            };
            return Finding::compatible(code, g, format!("{what} {shown}: {how}"), v.source);
        }
        if deps::major(c) == Some(v.major()) {
            return Finding::likely_compatible(
                code,
                g,
                format!(
                    "{what} {shown}: likely compatible — the same major version as the verified {}; this version itself is not verified",
                    v.version
                ),
                v.source,
            );
        }
        return not_verified();
    }
    match range.and_then(deps::major) {
        Some(m) if m == v.major() => Finding::likely_compatible(
            code,
            g,
            format!(
                "{what} {shown}: likely compatible — the same major version as the verified {}; the version it resolves to is not known and is not verified itself",
                v.version
            ),
            v.source,
        ),
        Some(_) => not_verified(),
        None => Finding::unknown(
            code,
            g,
            format!("{what} {shown}: no major version is pinned, so it is not known whether a verified version is used — manual verification required"),
        )
        .detail(format!("hidane verified {name} {}", v.version)),
    }
}

/// The version a dependency is known to be: resolved by a lockfile, or pinned exactly.
fn concrete(dep: &deps::Dependency) -> Option<&str> {
    dep.resolved
        .as_deref()
        .or_else(|| dep.declared.as_deref().and_then(deps::exact))
}

/// The dependency findings, and the status of every firebase-tools version found (the
/// Emulator UI's version follows it).
fn dependency_findings(facts: &Facts, ci: &ci::Ci, out: &mut Vec<Finding>) -> Vec<Status> {
    let g = Group::Dependencies;
    let mut tools_seen = false;
    let mut tools = Vec::new();
    for dep in &facts.dependencies {
        let shown = dep
            .resolved
            .clone()
            .or_else(|| dep.declared.clone())
            .map(|v| format!("{v} "))
            .unwrap_or_default();
        let shown = format!("{shown}({})", dep.file);
        let exact = concrete(dep);
        let declared = dep.declared.as_deref();
        let finding = match (dep.ecosystem, dep.name.as_str()) {
            ("npm", "hidane") => Some(Finding::info(
                "dependency.hidane",
                g,
                format!("hidane {shown} is a dependency: npm scripts can run `hidane exec`"),
            )),
            ("npm", "firebase-functions") => None,
            ("npm", "@google-cloud/datastore")
            | ("pypi", "google-cloud-datastore")
            | ("go", "cloud.google.com/go/datastore")
            | ("maven", "com.google.cloud:google-cloud-datastore") => Some(
                Finding::unknown(
                    "dependency.datastore",
                    g,
                    format!("{} {shown}: hidane serves Datastore mode (entities, transactions, queries, aggregations, GQL, index enforcement, export and import; the Node.js, Python, Java and Go clients verified, docs/sdk-certification.md) (#81); if these clients use the Firestore emulator in Datastore mode — manual verification required", dep.name),
                )
                .with_source(verified::GAP_DATASTORE),
            ),
            ("maven", "com.google.firebase:firebase-firestore") => Some(
                Finding::unknown(
                    "dependency.sdk",
                    g,
                    format!("{} {shown}: not verified with hidane — manual verification required", label(dep.ecosystem, &dep.name)),
                )
                .detail("docs/compatibility.md lists the Android SDK as not verified (same gRPC transport)"),
            ),
            ("pub", _) => {
                let web = dep.platforms.iter().any(|p| p == "web");
                let native: Vec<&str> = dep
                    .platforms
                    .iter()
                    .filter(|p| *p != "web")
                    .map(String::as_str)
                    .collect();
                if web {
                    let mut f = judge("dependency.sdk", dep.ecosystem, &dep.name, &format!("{shown} on the web"), exact, declared);
                    f = f.at(&dep.file, dep.line);
                    out.push(f);
                }
                if !native.is_empty() || dep.platforms.is_empty() {
                    let platforms = if native.is_empty() {
                        "(no platform directory found)".to_owned()
                    } else {
                        format!("on {}", native.join(", "))
                    };
                    Some(
                        Finding::unknown(
                            "dependency.sdk",
                            g,
                            format!("{} {shown} {platforms}: not verified with hidane — manual verification required", label(dep.ecosystem, &dep.name)),
                        )
                        .detail("Flutter on Android, iOS, macOS and Windows uses the native SDKs (gRPC), which are not verified yet (docs/compatibility.md)"),
                    )
                } else {
                    None
                }
            }
            (_, _) if dep.engine.is_some() => {
                let (engine, engine_version) = dep.engine.clone().unwrap_or_default();
                Some(match engine_version {
                    Some(ev) => {
                        let mut f = judge("dependency.sdk", dep.ecosystem, &engine, &ev, Some(&ev), None);
                        f.title = format!("{} {shown}, whose Firestore client is {}", label(dep.ecosystem, &dep.name), f.title);
                        f
                    }
                    None => Finding::unknown(
                        "dependency.sdk",
                        g,
                        format!(
                            "{} {shown}: its Firestore client, {engine}, is not in a lockfile — not verified with hidane, manual verification required",
                            label(dep.ecosystem, &dep.name)
                        ),
                    )
                    .details(
                        verified::verified(dep.ecosystem, &engine)
                            .map(|v| format!("hidane verified {engine} {}", v.version)),
                    ),
                })
            }
            ("maven", "com.google.firebase:firebase-admin") => Some(
                Finding::unknown(
                    "dependency.sdk",
                    g,
                    format!("{} {shown}: its Firestore client (google-cloud-firestore) is not declared — not verified with hidane, manual verification required", label(dep.ecosystem, &dep.name)),
                )
                .detail("hidane verified com.google.cloud:google-cloud-firestore 3.49.0"),
            ),
            (eco, name) => {
                let finding = judge("dependency.sdk", eco, name, &shown, exact, declared);
                if name == "firebase-tools" {
                    tools_seen = true;
                    tools.push(finding.status);
                }
                Some(finding)
            }
        };
        if let Some(finding) = finding {
            out.push(finding.at(&dep.file, dep.line));
        }
    }
    // firebase-tools installed in CI, or named by a launcher (`npx firebase-tools@15`).
    let mut ci_tools: Vec<(String, usize, Option<String>)> = ci.firebase_tools.clone();
    for c in &ci.commands {
        if let Some(v) = &c.firebase_tools {
            ci_tools.push((c.file.clone(), c.line, Some(v.clone())));
        }
    }
    for (file, line, version) in &ci_tools {
        tools_seen = true;
        let finding = match version.as_deref() {
            Some(v) if v != "latest" => judge(
                "dependency.sdk",
                "npm",
                "firebase-tools",
                &format!("{v} ({file})"),
                deps::exact(v),
                Some(v),
            ),
            _ => Finding::unknown(
                "dependency.sdk",
                g,
                format!(
                    "firebase-tools ({file}) is installed without a version: hidane exec is verified with firebase-tools 15.33.0 — manual verification required"
                ),
            ),
        };
        tools.push(finding.status);
        out.push(finding.at(file, Some(*line)));
    }
    if !tools_seen && !ci.commands.is_empty() {
        out.push(Finding::unknown(
            "dependency.firebase-tools",
            g,
            "The firebase-tools version is not known (it is in no package.json and no CI step installs it): hidane exec is verified with firebase-tools 15.33.0 — manual verification required",
        ));
    }
    tools
}

fn workflow_findings(facts: &Facts, ci: &ci::Ci, out: &mut Vec<Finding>) {
    let g = Group::Workflows;
    let firestore_port = facts
        .firebase
        .iter()
        .find_map(|f| f.firestore.as_ref())
        .map(|fs| fs.port);
    let hidane_dependency = facts
        .dependencies
        .iter()
        .any(|d| d.ecosystem == "npm" && d.name == "hidane");
    for workflow in &facts.workflows {
        let file = workflow.file.as_str();
        for job in &workflow.jobs {
            if job.emulators.is_empty() {
                continue;
            }
            let commands: Vec<&ci::EmulatorCommand> = job
                .emulators
                .iter()
                .map(|&i| &facts.emulator_commands[i])
                .collect();
            let all_wrapped = commands.iter().all(|c| c.under_hidane);
            let lines = commands
                .iter()
                .map(|c| format!("{}:{}: {}", c.file, c.line, c.command));
            let finding = if all_wrapped {
                Finding::ok(
                    "workflow.emulators",
                    g,
                    format!(
                        "{file}, job `{}`: runs the emulators under hidane exec",
                        job.id
                    ),
                )
            } else {
                Finding::info(
                    "workflow.emulators",
                    g,
                    format!("{file}, job `{}`: starts the emulators", job.id),
                )
            };
            out.push(finding.at(file, Some(job.line)).details(lines));
            if commands.iter().any(|c| c.under_hidane) && !job.setup_hidane && !hidane_dependency {
                out.push(
                    Finding::note(
                        "workflow.hidane-missing",
                        g,
                        format!("{file}, job `{}`: runs `hidane exec` but installs no hidane (hidane-dev/setup-hidane, or hidane as an npm dependency)", job.id),
                    )
                    .at(file, Some(job.line)),
                );
            }
            if job.other_shell {
                out.push(
                    Finding::unknown(
                        "workflow.shell",
                        g,
                        format!("{file}, job `{}`: starts the emulators on Windows or in a shell other than bash or sh, where hidane exec is not verified — manual verification required", job.id),
                    )
                    .with_source(verified::WINDOWS)
                    .at(file, Some(job.line)),
                );
            }
            if !job.setup_java.is_empty() {
                match &job.java {
                    ci::Java::NotNeeded => out.push(
                        Finding::note(
                            "workflow.setup-java",
                            g,
                            format!("{file}, job `{}`: actions/setup-java is there only for the Firestore emulator; with hidane the job needs no Java", job.id),
                        )
                        .at(file, Some(job.line))
                        .detail("`hidane migrate` replaces it with hidane-dev/setup-hidane@v1 when it can"),
                    ),
                    ci::Java::Needed(reasons) => out.push(
                        Finding::info(
                            "workflow.setup-java",
                            g,
                            format!("{file}, job `{}`: keep actions/setup-java: {} still {} Java", job.id, reasons.join(", "), if reasons.len() == 1 { "needs" } else { "need" }),
                        )
                        .at(file, Some(job.line)),
                    ),
                    ci::Java::Unknown(reasons) => out.push(
                        Finding::unknown(
                            "workflow.setup-java",
                            g,
                            format!("{file}, job `{}`: whether the job still needs Java is not known — manual verification required", job.id),
                        )
                        .at(file, Some(job.line))
                        .details(reasons.iter().cloned()),
                    ),
                    ci::Java::NotApplicable => {}
                }
            }
            for host in &job.emulator_host {
                let port = host
                    .rsplit_once(':')
                    .and_then(|(_, p)| p.parse::<u16>().ok());
                let finding = match (port, firestore_port) {
                    (Some(p), Some(fp)) if p != fp => Finding::note(
                        "workflow.emulator-host",
                        g,
                        format!(
                            "{file}, job `{}`: FIRESTORE_EMULATOR_HOST={host} does not match the port in firebase.json ({fp})",
                            job.id
                        ),
                    ),
                    _ => Finding::ok(
                        "workflow.emulator-host",
                        g,
                        format!(
                            "{file}, job `{}`: FIRESTORE_EMULATOR_HOST={host} stays as it is: hidane serves the same address",
                            job.id
                        ),
                    ),
                };
                out.push(finding.at(file, Some(job.line)));
            }
            if job.caches_emulators {
                out.push(
                    Finding::info(
                        "workflow.emulator-cache",
                        g,
                        format!("{file}, job `{}`: caches ~/.cache/firebase/emulators; under hidane exec, firebase-tools no longer downloads the Firestore emulator (the Emulator UI and other emulators still are)", job.id),
                    )
                    .at(file, Some(job.line)),
                );
            }
        }
    }
    if facts.emulator_commands.iter().any(|c| c.import || c.export) {
        out.push(Finding::compatible(
            "emulators.import-export",
            Group::Scripts,
            format!("Imports and exports: {}", verified::EXPORT_IMPORT.0),
            verified::EXPORT_IMPORT.1,
        ));
    }
    for note in &ci.notes {
        let group = if note.file.starts_with(".github/workflows/") {
            g
        } else {
            Group::Scripts
        };
        let finding = match note.kind {
            "gcloud-datastore" => Finding::unknown(
                "gcloud.datastore-mode",
                group,
                format!(
                    "{}:{}: hidane serves Datastore mode (entities, transactions, queries, aggregations, GQL, index enforcement, export and import; the Node.js, Python, Java and Go clients verified, docs/sdk-certification.md) (#81) — manual verification required",
                    note.file, note.line
                ),
            )
            .with_source(verified::GAP_DATASTORE),
            "gcloud-firestore" => Finding::unknown(
                "gcloud.firestore",
                group,
                format!(
                    "{}:{}: `gcloud emulators firestore` runs the official emulator directly; replace it with `hidane --host … --port …` by hand — manual change suggested",
                    note.file, note.line
                ),
            ),
            _ => Finding::info(
                "gcloud.emulators",
                group,
                format!(
                    "{}:{}: a gcloud emulator, which hidane does not replace",
                    note.file, note.line
                ),
            ),
        };
        out.push(
            finding
                .at(&note.file, Some(note.line))
                .detail(note.text.clone()),
        );
    }
}

fn script_findings(facts: &Facts, out: &mut Vec<Finding>) {
    let g = Group::Scripts;
    for script in &facts.scripts {
        let commands: Vec<&ci::EmulatorCommand> = script
            .emulators
            .iter()
            .map(|&i| &facts.emulator_commands[i])
            .collect();
        let finding = if commands.iter().all(|c| c.under_hidane) {
            Finding::ok(
                "script.emulators",
                g,
                format!(
                    "{}: script `{}` runs the emulators under hidane exec",
                    script.file, script.name
                ),
            )
        } else {
            Finding::info(
                "script.emulators",
                g,
                format!(
                    "{}: script `{}` starts the emulators",
                    script.file, script.name
                ),
            )
        };
        out.push(
            finding
                .at(&script.file, Some(script.line))
                .detail(script.command.clone()),
        );
    }
    // Emulator commands in shell scripts.
    for command in &facts.emulator_commands {
        let in_script = facts.scripts.iter().any(|s| s.file == command.file);
        let in_workflow = command.file.starts_with(".github/workflows/");
        if in_script || in_workflow || !command.editable {
            continue;
        }
        let finding = if command.under_hidane {
            Finding::ok(
                "script.emulators",
                g,
                format!(
                    "{}:{}: runs the emulators under hidane exec",
                    command.file, command.line
                ),
            )
        } else {
            Finding::info(
                "script.emulators",
                g,
                format!("{}:{}: starts the emulators", command.file, command.line),
            )
        };
        out.push(
            finding
                .at(&command.file, Some(command.line))
                .detail(command.command.clone()),
        );
    }
    for unread in &facts.unread {
        let group = if unread.file.starts_with(".github/workflows/") {
            Group::Workflows
        } else {
            g
        };
        out.push(
            Finding::unknown(
                "manual",
                group,
                format!(
                    "{}:{}: {} — manual change suggested",
                    unread.file, unread.line, unread.reason
                ),
            )
            .at(&unread.file, Some(unread.line))
            .detail(unread.text.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use files::Memory;

    #[test]
    fn an_empty_directory() {
        let report = scan(&Memory::new());
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "firebase-json.missing");
        assert!(!report.has_incompatibility());
    }

    #[test]
    fn compatible_findings_name_their_source() {
        let tree: Memory = [
            ("firebase.json", r#"{"firestore": {"rules": "firestore.rules", "edition": "enterprise"}}"#),
            ("firestore.rules", "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{d=**} {\n      allow read: if true;\n    }\n  }\n}\n"),
            ("package.json", r#"{"devDependencies": {"firebase": "13.0.0", "firebase-tools": "^14.0.0"}}"#),
        ]
        .into_iter()
        .collect();
        let report = scan(&tree);
        for f in &report.findings {
            if matches!(
                f.status,
                Status::Compatible | Status::LikelyCompatible | Status::Incompatible
            ) {
                assert!(f.source.is_some(), "{f:?}");
            }
        }
        let codes: Vec<_> = report.findings.iter().map(|f| (f.code, f.status)).collect();
        assert!(codes.contains(&("firestore.enterprise-edition", Status::Compatible)));
        assert!(
            codes.contains(&("rules.compile", Status::Compatible)),
            "{:#?}",
            report.facts.rules
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.status == Status::Unknown
                    && f.title.starts_with("firebase-tools ^14.0.0"))
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.status == Status::Compatible
                    && f.title.starts_with("Firebase JS SDK (firebase) 13.0.0"))
        );
        assert!(!report.has_incompatibility());
    }

    /// `compatible` takes the verified major and minor versions, of a version known exactly;
    /// the same major version alone, or a range, is at most `likely-compatible`.
    #[test]
    fn versions_are_compatible_only_as_verified() {
        let tree: Memory = [
            ("firebase.json", r#"{"firestore": {}, "emulators": {"ui": {}}}"#),
            (
                "package.json",
                r#"{"devDependencies": {"firebase": "13.0.0", "@firebase/rules-unit-testing": "6.0.3", "@google-cloud/firestore": "9.4.0", "firebase-tools": "^15.33.0"}}"#,
            ),
            ("requirements.txt", "google-cloud-firestore==2.35.0\n"),
            ("go.mod", "module x\n\nrequire cloud.google.com/go/firestore v1.26.3\n"),
            ("pom.xml", "<dependency><groupId>com.google.cloud</groupId><artifactId>google-cloud-firestore</artifactId><version>4.0.0</version></dependency>"),
        ]
        .into_iter()
        .collect();
        let report = scan(&tree);
        let status = |prefix: &str| {
            report
                .findings
                .iter()
                .find(|f| f.title.starts_with(prefix))
                .map(|f| f.status)
                .unwrap_or_else(|| panic!("{prefix}: {:#?}", report.findings))
        };
        assert_eq!(
            status("Firebase JS SDK (firebase) 13.0.0"),
            Status::Compatible
        );
        assert_eq!(
            status("@firebase/rules-unit-testing 6.0.3"),
            Status::Compatible
        );
        assert_eq!(
            status("@google-cloud/firestore 9.4.0"),
            Status::LikelyCompatible
        );
        assert_eq!(status("firebase-tools ^15.33.0"), Status::LikelyCompatible);
        assert_eq!(
            status("google-cloud-firestore (Python) 2.35.0"),
            Status::LikelyCompatible
        );
        assert_eq!(
            status("cloud.google.com/go/firestore 1.26.3"),
            Status::Compatible
        );
        assert_eq!(
            status("google-cloud-firestore (Java) 4.0.0"),
            Status::Unknown
        );
        // The Emulator UI follows firebase-tools.
        assert_eq!(status("Emulator UI"), Status::LikelyCompatible);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.title.contains("only the patch version differs"))
        );
        assert_eq!(report.summary.likely_compatible, 4);
        assert!(!report.has_incompatibility());
    }
}
