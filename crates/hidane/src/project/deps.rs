//! The Firestore SDKs and tools a project depends on, from its manifests and lockfiles:
//! `package.json` with `package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`,
//! `pnpm-lock.yaml` or `bun.lock`; `requirements*.txt`, `pyproject.toml` with `poetry.lock`,
//! `uv.lock` or `pdm.lock`; `go.mod`; `pom.xml`, `build.gradle(.kts)`; `pubspec.yaml` with
//! `pubspec.lock`.

use serde::Serialize;

use super::{
    files::{self, Tree},
    json,
    text::{Mapped, line_of},
    yaml,
};

/// npm packages the scan reports.
pub const NPM: [&str; 8] = [
    "firebase",
    "firebase-admin",
    "@google-cloud/firestore",
    "@firebase/rules-unit-testing",
    "firebase-tools",
    "hidane",
    "@google-cloud/datastore",
    "firebase-functions",
];
const PYPI: [&str; 3] = [
    "google-cloud-firestore",
    "firebase-admin",
    "google-cloud-datastore",
];
const GO: [&str; 3] = [
    "cloud.google.com/go/firestore",
    "firebase.google.com/go/v4",
    "cloud.google.com/go/datastore",
];
const MAVEN: [&str; 4] = [
    "com.google.cloud:google-cloud-firestore",
    "com.google.firebase:firebase-admin",
    "com.google.firebase:firebase-firestore",
    "com.google.cloud:google-cloud-datastore",
];
const PUB: [&str; 1] = ["cloud_firestore"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Dependency {
    /// `npm`, `pypi`, `go`, `maven`, `pub`.
    pub ecosystem: &'static str,
    pub name: String,
    /// The manifest that names it.
    pub file: String,
    pub line: Option<usize>,
    /// The version or range as written, if any.
    pub declared: Option<String>,
    /// The version a lockfile resolves it to.
    pub resolved: Option<String>,
    pub lockfile: Option<String>,
    /// For a package whose Firestore client is another package (`firebase-admin`): that
    /// package and the version the lockfile resolves it to.
    pub engine: Option<(String, Option<String>)>,
    /// For a Flutter package: the platform directories next to `pubspec.yaml`.
    pub platforms: Vec<String>,
}

/// A `package.json`: its name, its scripts and where each is.
#[derive(Debug, Clone)]
pub struct Package {
    pub file: String,
    pub name: Option<String>,
    pub scripts: Vec<(String, Mapped)>,
    /// The text of the file, for the line of each script.
    pub text: String,
}

impl Package {
    pub fn dir(&self) -> &str {
        files::parent(&self.file)
    }

    pub fn script(&self, name: &str) -> Option<&Mapped> {
        self.scripts
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
    }

    pub fn line(&self, script: &Mapped) -> usize {
        line_of(&self.text, script.offset(0))
    }
}

pub fn scan(tree: &dyn Tree, files: &[String]) -> (Vec<Dependency>, Vec<Package>) {
    let mut deps = Vec::new();
    let mut packages = Vec::new();
    for file in files {
        let name = files::file_name(file);
        match name {
            "package.json" => {
                if let Some(text) = tree.read(file) {
                    npm(tree, file, &text, &mut deps, &mut packages);
                }
            }
            "go.mod" => {
                if let Some(text) = tree.read(file) {
                    go(file, &text, &mut deps);
                }
            }
            "pom.xml" => {
                if let Some(text) = tree.read(file) {
                    maven(file, &text, &mut deps);
                }
            }
            "build.gradle" | "build.gradle.kts" => {
                if let Some(text) = tree.read(file) {
                    gradle(file, &text, &mut deps);
                }
            }
            "pubspec.yaml" => {
                if let Some(text) = tree.read(file) {
                    pubspec(tree, file, &text, &mut deps);
                }
            }
            "pyproject.toml" => {
                if let Some(text) = tree.read(file) {
                    pyproject(tree, file, &text, &mut deps);
                }
            }
            n if n.starts_with("requirements") && n.ends_with(".txt")
                || files::parent(file).ends_with("requirements") && n.ends_with(".txt") =>
            {
                if let Some(text) = tree.read(file) {
                    requirements(tree, file, &text, &mut deps);
                }
            }
            _ => {}
        }
    }
    (deps, packages)
}

fn npm(
    tree: &dyn Tree,
    file: &str,
    text: &str,
    deps: &mut Vec<Dependency>,
    packages: &mut Vec<Package>,
) {
    let Ok(doc) = json::parse(text) else {
        return;
    };
    let scripts = doc
        .get("scripts")
        .map(|s| {
            s.entries()
                .iter()
                .filter_map(|(k, v)| Some((k.text.clone(), v.as_mapped()?.clone())))
                .collect()
        })
        .unwrap_or_default();
    packages.push(Package {
        file: file.to_owned(),
        name: doc.get("name").and_then(|n| n.as_str()).map(str::to_owned),
        scripts,
        text: text.to_owned(),
    });
    let locks = NpmLocks::find(tree, files::parent(file));
    let mut seen = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        let Some(list) = doc.get(section) else {
            continue;
        };
        for (key, value) in list.entries() {
            let name = key.text.as_str();
            if !NPM.contains(&name) || seen.contains(&name.to_owned()) {
                continue;
            }
            seen.push(name.to_owned());
            let declared = value.as_str().map(str::to_owned);
            let resolved = locks.version(name, declared.as_deref());
            let engine = (name == "firebase-admin").then(|| {
                let engine = "@google-cloud/firestore";
                (engine.to_owned(), locks.version(engine, None))
            });
            deps.push(Dependency {
                ecosystem: "npm",
                name: name.to_owned(),
                file: file.to_owned(),
                line: Some(line_of(text, key.offset(0))),
                declared,
                resolved,
                lockfile: locks.file.clone(),
                engine,
                platforms: Vec::new(),
            });
        }
    }
}

/// The lockfile of an npm project, in its directory or the nearest one above (workspaces).
struct NpmLocks {
    file: Option<String>,
    /// The package directory relative to the lockfile's.
    rel: String,
    lock: Lock,
}

enum Lock {
    None,
    /// `package-lock.json` / `npm-shrinkwrap.json`.
    Npm(json::Node),
    /// `yarn.lock`: (descriptors, version) per entry.
    Yarn(Vec<(Vec<String>, String)>),
    /// `pnpm-lock.yaml`.
    Pnpm(yaml::Doc),
    /// `bun.lock`.
    Bun(String),
}

impl NpmLocks {
    fn find(tree: &dyn Tree, dir: &str) -> Self {
        let mut current = dir.to_owned();
        loop {
            for name in [
                "package-lock.json",
                "npm-shrinkwrap.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "bun.lock",
            ] {
                let Some(file) = files::join(&current, name) else {
                    continue;
                };
                let Some(text) = tree.read(&file) else {
                    continue;
                };
                let lock = match name {
                    "yarn.lock" => Lock::Yarn(yarn_entries(&text)),
                    "pnpm-lock.yaml" => Lock::Pnpm(yaml::parse(&text)),
                    "bun.lock" => Lock::Bun(text),
                    _ => match json::parse(&text) {
                        Ok(doc) => Lock::Npm(doc),
                        Err(_) => Lock::None,
                    },
                };
                let rel = dir
                    .strip_prefix(&current)
                    .unwrap_or(dir)
                    .trim_start_matches('/')
                    .to_owned();
                return Self {
                    file: Some(file),
                    rel,
                    lock,
                };
            }
            if current.is_empty() {
                return Self {
                    file: None,
                    rel: String::new(),
                    lock: Lock::None,
                };
            }
            current = files::parent(&current).to_owned();
        }
    }

    /// The version `name` resolves to for this package.
    fn version(&self, name: &str, declared: Option<&str>) -> Option<String> {
        match &self.lock {
            Lock::None => None,
            Lock::Npm(doc) => {
                if let Some(packages) = doc.get("packages") {
                    let nested = if self.rel.is_empty() {
                        None
                    } else {
                        Some(format!("{}/node_modules/{name}", self.rel))
                    };
                    let hoisted = format!("node_modules/{name}");
                    let found = nested
                        .iter()
                        .chain(std::iter::once(&hoisted))
                        .find_map(|key| packages.get(key))
                        .or_else(|| {
                            // Nested under the package that needs it.
                            let suffix = format!("/node_modules/{name}");
                            packages
                                .entries()
                                .iter()
                                .find(|(k, _)| k.text.ends_with(&suffix))
                                .map(|(_, v)| v)
                        });
                    found
                        .and_then(|p| p.get("version"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                } else {
                    // lockfileVersion 1.
                    doc.at(&["dependencies", name, "version"])
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                }
            }
            Lock::Yarn(entries) => {
                let versions: Vec<&String> = entries
                    .iter()
                    .filter(|(descriptors, _)| {
                        descriptors.iter().any(|d| descriptor_name(d) == name)
                    })
                    .map(|(_, v)| v)
                    .collect();
                if let Some(range) = declared {
                    let wanted = [format!("{name}@{range}"), format!("{name}@npm:{range}")];
                    if let Some((_, v)) = entries
                        .iter()
                        .find(|(descriptors, _)| descriptors.iter().any(|d| wanted.contains(d)))
                    {
                        return Some(v.clone());
                    }
                }
                match versions.as_slice() {
                    [one] => Some((*one).clone()),
                    _ => None,
                }
            }
            Lock::Pnpm(doc) => {
                let root = doc.root.as_ref()?;
                let importer = if self.rel.is_empty() { "." } else { &self.rel };
                let sections = ["dependencies", "devDependencies", "optionalDependencies"];
                let direct = root
                    .get("importers")
                    .and_then(|i| i.get(importer))
                    .into_iter()
                    .chain(std::iter::once(root))
                    .flat_map(|scope| sections.iter().filter_map(move |s| scope.get(s)))
                    .find_map(|deps| deps.get(name))
                    .and_then(|d| d.get("version").or(Some(d)))
                    .and_then(|v| v.as_str())
                    .map(|v| v.split('(').next().unwrap_or(v).to_owned());
                direct.or_else(|| {
                    let mut versions = Vec::new();
                    for section in ["packages", "snapshots"] {
                        for entry in root.get(section).map(|p| p.entries()).unwrap_or_default() {
                            let key = entry.key.trim_start_matches('/');
                            if let Some(rest) = key.strip_prefix(name)
                                && let Some(version) = rest.strip_prefix('@')
                            {
                                let version = version.split('(').next().unwrap_or(version);
                                if !versions.contains(&version.to_owned()) {
                                    versions.push(version.to_owned());
                                }
                            }
                        }
                    }
                    (versions.len() == 1).then(|| versions.remove(0))
                })
            }
            Lock::Bun(text) => {
                let needle = format!("\"{name}\": [\"{name}@");
                let at = text.find(&needle)? + needle.len();
                let end = text[at..].find('"')? + at;
                Some(text[at..end].to_owned())
            }
        }
    }
}

/// The package name of a yarn descriptor (`@scope/name@^1`, `name@npm:^1`).
fn descriptor_name(descriptor: &str) -> &str {
    let search_from = usize::from(descriptor.starts_with('@'));
    match descriptor[search_from..].find('@') {
        Some(at) => &descriptor[..search_from + at],
        None => descriptor,
    }
}

fn yarn_entries(text: &str) -> Vec<(Vec<String>, String)> {
    let mut entries = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') {
            current = line.strip_suffix(':').map(|header| {
                header
                    .split(", ")
                    .map(|d| d.trim().trim_matches('"').to_owned())
                    .collect()
            });
            continue;
        }
        let trimmed = line.trim();
        let version = trimmed
            .strip_prefix("version ")
            .or_else(|| trimmed.strip_prefix("version: "));
        if let (Some(descriptors), Some(version)) = (&current, version)
            && line.starts_with("  ")
            && !line.starts_with("    ")
        {
            entries.push((descriptors.clone(), version.trim_matches('"').to_owned()));
        }
    }
    entries
}

fn go(file: &str, text: &str, deps: &mut Vec<Dependency>) {
    let mut in_require = false;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.split("//").next().unwrap_or("").trim();
        if trimmed.starts_with("require (") {
            in_require = true;
            continue;
        }
        if in_require && trimmed == ")" {
            in_require = false;
            continue;
        }
        let spec = if in_require {
            trimmed
        } else if let Some(rest) = trimmed.strip_prefix("require ") {
            rest.trim()
        } else {
            continue;
        };
        let mut parts = spec.split_whitespace();
        let (Some(module), Some(version)) = (parts.next(), parts.next()) else {
            continue;
        };
        if GO.contains(&module) {
            deps.push(Dependency {
                ecosystem: "go",
                name: module.to_owned(),
                file: file.to_owned(),
                line: Some(i + 1),
                declared: Some(version.to_owned()),
                resolved: Some(version.trim_start_matches('v').to_owned()),
                lockfile: None,
                engine: None,
                platforms: Vec::new(),
            });
        }
    }
    // firebase-admin's Firestore client is the firestore module go.mod also requires.
    let firestore = deps
        .iter()
        .find(|d| d.file == file && d.name == GO[0])
        .and_then(|d| d.resolved.clone());
    for dep in deps
        .iter_mut()
        .filter(|d| d.file == file && d.name == GO[1])
    {
        dep.engine = Some((GO[0].to_owned(), firestore.clone()));
    }
}

/// The text of the first `<tag>…</tag>` in `xml`.
fn xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

fn maven(file: &str, text: &str, deps: &mut Vec<Dependency>) {
    let mut from = 0;
    while let Some(at) = text[from..].find("<dependency>") {
        let start = from + at;
        let Some(len) = text[start..].find("</dependency>") else {
            break;
        };
        let block = &text[start..start + len];
        from = start + len;
        let (Some(group), Some(artifact)) =
            (xml_tag(block, "groupId"), xml_tag(block, "artifactId"))
        else {
            continue;
        };
        let name = format!("{group}:{artifact}");
        if !MAVEN.contains(&name.as_str()) {
            continue;
        }
        let version = xml_tag(block, "version").map(|v| {
            // `${property}`, from `<properties>`.
            match v.strip_prefix("${").and_then(|p| p.strip_suffix('}')) {
                Some(property) => xml_tag(text, property).unwrap_or(v).to_owned(),
                None => v.to_owned(),
            }
        });
        deps.push(Dependency {
            ecosystem: "maven",
            name,
            file: file.to_owned(),
            line: Some(line_of(text, start)),
            resolved: version.clone().filter(|v| !v.contains("${")),
            declared: version,
            lockfile: None,
            engine: None,
            platforms: Vec::new(),
        });
    }
}

fn gradle(file: &str, text: &str, deps: &mut Vec<Dependency>) {
    for (i, line) in text.lines().enumerate() {
        for name in MAVEN {
            let Some(at) = line.find(name) else {
                continue;
            };
            let after = &line[at + name.len()..];
            let version = after
                .strip_prefix(':')
                .map(|v| {
                    v.chars()
                        .take_while(|c| !matches!(c, '"' | '\'' | ')' | ' ' | ':' | '@'))
                        .collect::<String>()
                })
                .filter(|v| !v.is_empty());
            deps.push(Dependency {
                ecosystem: "maven",
                name: name.to_owned(),
                file: file.to_owned(),
                line: Some(i + 1),
                resolved: version.clone().filter(|v| !v.contains('$')),
                declared: version,
                lockfile: None,
                engine: None,
                platforms: Vec::new(),
            });
        }
    }
}

fn pubspec(tree: &dyn Tree, file: &str, text: &str, deps: &mut Vec<Dependency>) {
    let doc = yaml::parse(text);
    let Some(root) = &doc.root else {
        return;
    };
    let dir = files::parent(file);
    let lock = files::join(dir, "pubspec.lock").and_then(|f| Some((tree.read(&f)?, f)));
    let lock_doc = lock.as_ref().map(|(text, _)| yaml::parse(text));
    let platforms: Vec<String> = tree
        .list(dir)
        .into_iter()
        .filter(|e| {
            e.is_dir
                && matches!(
                    e.name.as_str(),
                    "android" | "ios" | "macos" | "windows" | "linux" | "web"
                )
        })
        .map(|e| e.name)
        .collect();
    for section in ["dependencies", "dev_dependencies"] {
        for entry in root.get(section).map(|d| d.entries()).unwrap_or_default() {
            if !PUB.contains(&entry.key.as_str()) {
                continue;
            }
            let declared = entry
                .value
                .as_str()
                .or_else(|| entry.value.get("version").and_then(|v| v.as_str()))
                .map(str::to_owned);
            let resolved = lock_doc
                .as_ref()
                .and_then(|d| d.root.as_ref())
                .and_then(|r| r.get("packages"))
                .and_then(|p| p.get(&entry.key))
                .and_then(|p| p.get("version"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            deps.push(Dependency {
                ecosystem: "pub",
                name: entry.key.clone(),
                file: file.to_owned(),
                line: Some(entry.line + 1),
                declared,
                lockfile: resolved.as_ref().and(lock.as_ref()).map(|(_, f)| f.clone()),
                resolved,
                engine: None,
                platforms: platforms.clone(),
            });
        }
    }
}

/// `name[extras] <spec>` of a PEP 508 requirement, the name normalised.
fn pep508(requirement: &str) -> Option<(String, String)> {
    let requirement = requirement.split(';').next()?.trim();
    let end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(requirement.len());
    let name = requirement[..end]
        .to_ascii_lowercase()
        .replace(['_', '.'], "-");
    let mut spec = requirement[end..].trim();
    if spec.starts_with('[') {
        spec = spec.find(']').map_or("", |e| spec[e + 1..].trim());
    }
    (!name.is_empty()).then(|| (name, spec.trim_matches(['(', ')']).trim().to_owned()))
}

/// The version a Python lockfile next to `file` resolves `name` to.
fn python_lock(tree: &dyn Tree, file: &str, name: &str) -> Option<(String, String)> {
    let dir = files::parent(file);
    for lock in ["poetry.lock", "uv.lock", "pdm.lock"] {
        let path = files::join(dir, lock)?;
        let Some(text) = tree.read(&path) else {
            continue;
        };
        let wanted = format!("name = \"{name}\"");
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            if line.trim() == wanted {
                for next in lines.by_ref() {
                    if let Some(v) = next.trim().strip_prefix("version = ") {
                        return Some((v.trim_matches('"').to_owned(), path));
                    }
                    if next.starts_with('[') {
                        break;
                    }
                }
            }
        }
    }
    None
}

fn python(
    tree: &dyn Tree,
    file: &str,
    line: usize,
    name: &str,
    spec: &str,
    deps: &mut Vec<Dependency>,
) {
    if !PYPI.contains(&name) || deps.iter().any(|d| d.file == file && d.name == name) {
        return;
    }
    let pinned = spec.strip_prefix("==").map(|v| v.trim().to_owned());
    let lock = python_lock(tree, file, name);
    let engine = (name == "firebase-admin").then(|| {
        (
            PYPI[0].to_owned(),
            python_lock(tree, file, PYPI[0]).map(|(v, _)| v),
        )
    });
    deps.push(Dependency {
        ecosystem: "pypi",
        name: name.to_owned(),
        file: file.to_owned(),
        line: Some(line),
        declared: (!spec.is_empty()).then(|| spec.to_owned()),
        resolved: lock.as_ref().map(|(v, _)| v.clone()).or(pinned),
        lockfile: lock.map(|(_, f)| f),
        engine,
        platforms: Vec::new(),
    });
}

fn requirements(tree: &dyn Tree, file: &str, text: &str, deps: &mut Vec<Dependency>) {
    for (i, line) in text.lines().enumerate() {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with(['#', '-']) {
            continue;
        }
        if let Some((name, spec)) = pep508(line) {
            python(tree, file, i + 1, &name, &spec, deps);
        }
    }
}

fn pyproject(tree: &dyn Tree, file: &str, text: &str, deps: &mut Vec<Dependency>) {
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        // PEP 621: "name>=1,<2" in a list.
        let mut rest = trimmed;
        while let Some(open) = rest.find(['"', '\'']) {
            let quote = rest.as_bytes()[open] as char;
            let Some(close) = rest[open + 1..].find(quote) else {
                break;
            };
            let inner = &rest[open + 1..open + 1 + close];
            if let Some((name, spec)) = pep508(inner)
                && PYPI.contains(&name.as_str())
            {
                python(tree, file, i + 1, &name, &spec, deps);
            }
            rest = &rest[open + close + 2..];
        }
        // Poetry: name = "^1.2" or name = { version = "^1.2" }.
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key
                .trim()
                .trim_matches('"')
                .to_ascii_lowercase()
                .replace('_', "-");
            if PYPI.contains(&key.as_str()) {
                let value = value.trim();
                let spec = if value.starts_with('{') {
                    value
                        .split("version")
                        .nth(1)
                        .and_then(|v| v.split('"').nth(1))
                        .unwrap_or("")
                } else {
                    value.trim_matches(['"', '\''])
                };
                python(tree, file, i + 1, &key, &poetry_spec(spec), deps);
            }
        }
    }
}

/// A Poetry constraint (`^2.20`, `2.34.1`) in PEP 440 terms where they differ.
fn poetry_spec(spec: &str) -> String {
    if spec.starts_with(|c: char| c.is_ascii_digit()) {
        format!("=={spec}")
    } else {
        spec.to_owned()
    }
}

/// The major version a version or range pins, if it pins one: `13.0.0`, `^13.1`, `~13.0.0`,
/// `13.x`, `==2.34.1`, `~=2.34`, `>=2.20,<3`, `v1.26.0`. A range across majors, or without an
/// upper bound, pins none.
pub fn major(spec: &str) -> Option<u64> {
    let spec = spec.trim();
    if spec.is_empty() || spec.contains("||") || spec.contains(" - ") {
        return None;
    }
    let number = |s: &str| -> Option<u64> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let parts: Vec<&str> = spec
        .split([',', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut lower = None;
    let mut upper: Option<(u64, bool)> = None;
    let mut exact = None;
    for part in parts {
        let at = part.find(|c: char| c.is_ascii_digit() || c == 'v')?;
        let (op, version) = (&part[..at], &part[at..]);
        let n = number(version)?;
        match op {
            "" | "=" | "==" | "^" | "~" | "~=" | "~>" => exact = Some(n),
            ">=" | ">" => lower = Some(n),
            "<" => {
                // `<3` and `<3.0.0` end major 2; `<3.1` lets 3.0 in.
                let rest = version.trim_start_matches(['v', 'V']);
                let whole = rest.split('.').skip(1).all(|p| p.chars().all(|c| c == '0'));
                upper = Some((n, whole));
            }
            "<=" => upper = Some((n, false)),
            _ => return None,
        }
    }
    match (exact, lower, upper) {
        (Some(n), None, None) => Some(n),
        (Some(n), _, Some((u, whole))) if u > n || u == n && !whole => Some(n),
        (None, Some(l), Some((u, true))) if u == l + 1 => Some(l),
        (None, Some(l), Some((u, false))) if u == l => Some(l),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::files::Memory;

    #[test]
    fn majors() {
        for (spec, expected) in [
            ("13.0.0", Some(13)),
            ("^13.1.0", Some(13)),
            ("~9.3", Some(9)),
            ("13.x", Some(13)),
            ("v1.26.0", Some(1)),
            ("==2.34.1", Some(2)),
            ("~=2.34", Some(2)),
            (">=2.20,<3", Some(2)),
            (">=2.20, <3.0.0", Some(2)),
            (">=2.20", None),
            (">=2,<4", None),
            ("^12 || ^13", None),
            ("latest", None),
            ("*", None),
            ("", None),
        ] {
            assert_eq!(major(spec), expected, "{spec}");
        }
    }

    #[test]
    fn npm_with_lockfiles() {
        let tree: Memory = [
            (
                "package.json",
                r#"{"name": "app", "devDependencies": {"firebase-tools": "^15.0.0", "firebase": "^13.0.0", "firebase-admin": "^13.0.0", "left-pad": "1"}, "scripts": {"test": "jest"}}"#,
            ),
            (
                "package-lock.json",
                r#"{"lockfileVersion": 3, "packages": {"": {}, "node_modules/firebase-tools": {"version": "15.33.0"}, "node_modules/firebase": {"version": "13.0.0"}, "node_modules/firebase-admin": {"version": "13.5.0"}, "node_modules/firebase-admin/node_modules/@google-cloud/firestore": {"version": "7.11.6"}}}"#,
            ),
            (
                "web/package.json",
                r#"{"dependencies": {"firebase": "^11.10.0"}}"#,
            ),
            (
                "web/yarn.lock",
                "# yarn lockfile v1\n\n\"firebase@^11.10.0\", firebase@^11.1.0:\n  version \"11.10.0\"\n  resolved \"x\"\n",
            ),
        ]
        .into_iter()
        .collect();
        let files = vec!["package.json".to_owned(), "web/package.json".to_owned()];
        let (deps, packages) = scan(&tree, &files);
        let got: Vec<_> = deps
            .iter()
            .map(|d| (d.file.as_str(), d.name.as_str(), d.resolved.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("package.json", "firebase-tools", Some("15.33.0")),
                ("package.json", "firebase", Some("13.0.0")),
                ("package.json", "firebase-admin", Some("13.5.0")),
                ("web/package.json", "firebase", Some("11.10.0")),
            ]
        );
        assert_eq!(
            deps[2].engine,
            Some(("@google-cloud/firestore".into(), Some("7.11.6".into())))
        );
        assert_eq!(packages[0].script("test").unwrap().text, "jest");
        assert_eq!(packages[0].name.as_deref(), Some("app"));
    }

    #[test]
    fn pnpm_and_other_ecosystems() {
        let tree: Memory = [
            ("package.json", r#"{"dependencies": {"firebase": "^13.0.0"}}"#),
            (
                "pnpm-lock.yaml",
                "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      firebase:\n        specifier: ^13.0.0\n        version: 13.0.0(react@18.0.0)\n",
            ),
            (
                "go.mod",
                "module x\n\nrequire (\n\tcloud.google.com/go/firestore v1.26.0 // indirect\n\tfirebase.google.com/go/v4 v4.18.0\n)\n",
            ),
            (
                "requirements.txt",
                "google-cloud-firestore==2.34.1  # pinned\nfirebase_admin>=6.5,<7\n",
            ),
            (
                "pom.xml",
                "<project><properties><firestore.version>3.49.0</firestore.version></properties><dependencies><dependency>\n<groupId>com.google.cloud</groupId>\n<artifactId>google-cloud-firestore</artifactId>\n<version>${firestore.version}</version></dependency></dependencies></project>",
            ),
            (
                "app/pubspec.yaml",
                "name: app\ndependencies:\n  cloud_firestore: ^6.10.0\n",
            ),
            ("app/web/index.html", ""),
            ("app/android/build.gradle", ""),
        ]
        .into_iter()
        .collect();
        let files = vec![
            "app/pubspec.yaml".to_owned(),
            "go.mod".to_owned(),
            "package.json".to_owned(),
            "pom.xml".to_owned(),
            "requirements.txt".to_owned(),
        ];
        let (deps, _) = scan(&tree, &files);
        let got: Vec<_> = deps
            .iter()
            .map(|d| {
                (
                    d.ecosystem,
                    d.name.as_str(),
                    d.declared.as_deref(),
                    d.resolved.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("pub", "cloud_firestore", Some("^6.10.0"), None),
                (
                    "go",
                    "cloud.google.com/go/firestore",
                    Some("v1.26.0"),
                    Some("1.26.0")
                ),
                (
                    "go",
                    "firebase.google.com/go/v4",
                    Some("v4.18.0"),
                    Some("4.18.0")
                ),
                ("npm", "firebase", Some("^13.0.0"), Some("13.0.0")),
                (
                    "maven",
                    "com.google.cloud:google-cloud-firestore",
                    Some("3.49.0"),
                    Some("3.49.0")
                ),
                (
                    "pypi",
                    "google-cloud-firestore",
                    Some("==2.34.1"),
                    Some("2.34.1")
                ),
                ("pypi", "firebase-admin", Some(">=6.5,<7"), None),
            ]
        );
        assert_eq!(deps[0].platforms, ["android", "web"]);
        assert_eq!(
            deps[2].engine,
            Some((
                "cloud.google.com/go/firestore".into(),
                Some("1.26.0".into())
            ))
        );
    }
}
