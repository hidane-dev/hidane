//! What `hidane report` does to every text it did not write itself before the text goes into a
//! report. A [`Redactor`] is told what it must never let through (the project's root, the home
//! directory, the machine's and the user's names, the values of secret environment variables,
//! the project IDs it knows of) and finds the rest by its shape:
//!
//! | What | Becomes |
//! |---|---|
//! | a private key block (`-----BEGIN … PRIVATE KEY-----`) | `<private-key>` |
//! | a value known to be secret (a secret environment variable's value) | `<redacted>` |
//! | credentials in a URL (`https://user:password@host`), except ssh's `git@` | `<credentials>` |
//! | a secret-looking query parameter (`?key=`, `?token=`, `?X-Goog-Signature=`) | `<redacted>` |
//! | an API key (`AIza…`) or a token (`ya29.`, `1//`, GitHub, Slack, Stripe, npm, AWS, JWTs, `Bearer …`) | `<api-key>`, `<token>` |
//! | the value of a secret-looking key (`password=`, `"apiKey": …`, `--token …`) | `<redacted>` |
//! | a project ID after `--project`, `-P`, `--project_id`, in `projects/<id>`, before `.firebaseapp.com`, `.web.app`, `.appspot.com`, `.firebaseio.com` and the like | `<project-id-N>` |
//! | a Firebase app ID (`1:1234:web:abcd`) | `<app-id>` |
//! | the project's root, home directories, temporary directories | `<project>`, `<home>`, `<temp>` |
//! | a project ID, host or name already seen, wherever it appears again | its placeholder |
//! | an e-mail address | `<email>` |
//!
//! IDs that start with `demo-` are kept: firebase-tools treats them as fake projects, and they
//! name nothing real. So are references to secrets (`$FIREBASE_TOKEN`, `${{ secrets.X }}`), the
//! emulator's `Bearer owner`, and loopback hosts. Every replacement is counted by [`Kind`], so
//! the report can say what it left out. Redacting a redacted text changes nothing.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// What a replacement stood for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    ProjectId,
    AppId,
    Email,
    ApiKey,
    Token,
    PrivateKey,
    UrlCredentials,
    Secret,
    ProjectPath,
    HomePath,
    TempPath,
    Host,
    HostName,
    UserName,
}

impl Kind {
    /// The summary's words for one and for several.
    pub fn noun(self) -> (&'static str, &'static str) {
        match self {
            Kind::ProjectId => ("project ID", "project IDs"),
            Kind::AppId => ("Firebase app ID", "Firebase app IDs"),
            Kind::Email => ("e-mail address", "e-mail addresses"),
            Kind::ApiKey => ("API key", "API keys"),
            Kind::Token => ("token", "tokens"),
            Kind::PrivateKey => ("private key", "private keys"),
            Kind::UrlCredentials => ("credential in a URL", "credentials in URLs"),
            Kind::Secret => ("secret value", "secret values"),
            Kind::ProjectPath => ("path in the project", "paths in the project"),
            Kind::HomePath => ("home directory path", "home directory paths"),
            Kind::TempPath => ("temporary directory path", "temporary directory paths"),
            Kind::Host => ("host other than localhost", "hosts other than localhost"),
            Kind::HostName => ("machine name", "machine names"),
            Kind::UserName => ("user name", "user names"),
        }
    }
}

/// Redacts texts, and remembers what it has been told and has found.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Absolute paths, `/`-separated, longest first, with their kind.
    paths: Vec<(String, Kind)>,
    /// Project IDs, in the order first seen: the first is `<project-id-1>`.
    projects: Vec<String>,
    /// `.firebaserc` aliases (`default`, `staging`), which are labels, not IDs.
    aliases: Vec<String>,
    /// Hosts other than loopback ones, lowercase.
    hosts: Vec<String>,
    /// Values replaced wherever they appear.
    secrets: Vec<String>,
    /// The machine's and the user's names, lowercase, replaced where they are whole words.
    names: Vec<(String, Kind)>,
    counts: BTreeMap<Kind, usize>,
}

/// Words of a key that make its value secret (`FIREBASE_TOKEN`, `apiKey`, `client_secret`).
const SECRET_WORDS: [&str; 33] = [
    "secret",
    "secrets",
    "token",
    "tokens",
    "password",
    "passwords",
    "passwd",
    "pwd",
    "pass",
    "passphrase",
    "credential",
    "credentials",
    "creds",
    "apikey",
    "key",
    "keys",
    "privatekey",
    "cookie",
    "cookies",
    "session",
    "sessionid",
    "signature",
    "sig",
    "authorization",
    "bearer",
    "dsn",
    "salt",
    "otp",
    "jwt",
    "cert",
    "certificate",
    "keystore",
    "webhook",
];

/// Parts that make a key secret wherever they are in it (`x-api-key`, `accessToken`).
const SECRET_PARTS: [&str; 11] = [
    "apikey",
    "privatekey",
    "accesskey",
    "secretkey",
    "clientsecret",
    "password",
    "passwd",
    "secret",
    "token",
    "credential",
    "authorization",
];

/// Names that identify no one: generic accounts, machines and CI runners, and the words of the
/// placeholders themselves.
const GENERIC_NAMES: [&str; 50] = [
    "root",
    "runner",
    "runneradmin",
    "ubuntu",
    "debian",
    "admin",
    "administrator",
    "user",
    "users",
    "guest",
    "nobody",
    "node",
    "vscode",
    "codespace",
    "codespaces",
    "ec2-user",
    "docker",
    "vagrant",
    "app",
    "dev",
    "developer",
    "test",
    "tests",
    "build",
    "builder",
    "jenkins",
    "circleci",
    "gitpod",
    "buildkite",
    "github",
    "firebase",
    "hidane",
    "java",
    "localhost",
    "local",
    "default",
    "home",
    "project",
    "temp",
    "tmp",
    "redacted",
    "token",
    "email",
    "host",
    "hostname",
    "credentials",
    "secret",
    "macbook",
    "macbook-pro",
    "macbook-air",
];

/// Token prefixes: the prefix, the shortest tail that makes it a token, and what it is.
const TOKENS: [(&str, usize, Kind); 26] = [
    ("AIza", 30, Kind::ApiKey),
    ("ya29.", 20, Kind::Token),
    ("1//", 20, Kind::Token),
    ("ghp_", 20, Kind::Token),
    ("gho_", 20, Kind::Token),
    ("ghu_", 20, Kind::Token),
    ("ghs_", 20, Kind::Token),
    ("ghr_", 20, Kind::Token),
    ("github_pat_", 20, Kind::Token),
    ("glpat-", 20, Kind::Token),
    ("xoxb-", 10, Kind::Token),
    ("xoxp-", 10, Kind::Token),
    ("xoxa-", 10, Kind::Token),
    ("xoxr-", 10, Kind::Token),
    ("xoxs-", 10, Kind::Token),
    ("sk_live_", 10, Kind::Token),
    ("sk_test_", 10, Kind::Token),
    ("rk_live_", 10, Kind::Token),
    ("rk_test_", 10, Kind::Token),
    ("sk-", 20, Kind::Token),
    ("AKIA", 16, Kind::Token),
    ("ASIA", 16, Kind::Token),
    ("npm_", 30, Kind::Token),
    ("pypi-", 20, Kind::Token),
    ("GOCSPX-", 10, Kind::Token),
    ("SG.", 20, Kind::Token),
];

/// Hosts whose first label names a project (or a site or database named after one), and
/// whether that label is the project ID itself.
const FIREBASE_HOSTS: [(&str, bool); 8] = [
    (".firebaseapp.com", true),
    (".web.app", true),
    (".appspot.com", true),
    (".firebaseio.com", true),
    (".firebasedatabase.app", true),
    (".firebasestorage.app", true),
    (".cloudfunctions.net", false),
    (".run.app", false),
];

/// Whether the value of `key` is a secret: `FIREBASE_TOKEN`, `apiKey`, `client_secret`,
/// `x-api-key`, `privateKeyId`, `"password"`.
pub fn secret_key(key: &str) -> bool {
    let words = words(key);
    let joined: String = words.concat();
    words.iter().any(|w| SECRET_WORDS.contains(&w.as_str()))
        || SECRET_PARTS.iter().any(|p| joined.contains(p))
}

/// The lowercase words of a key: split at anything but letters and digits, and where camel
/// case starts a word (`privateKeyId` → `private`, `key`, `id`; `HTTPServer` → `http`,
/// `server`).
fn words(key: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in key.split(|c: char| !c.is_ascii_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let starts = c.is_ascii_uppercase()
                && i > 0
                && (chars[i - 1].is_ascii_lowercase()
                    || chars[i - 1].is_ascii_digit()
                    || chars.get(i + 1).is_some_and(char::is_ascii_lowercase)
                        && chars[i - 1].is_ascii_uppercase());
            if starts && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c.to_ascii_lowercase());
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// A key without case and separators (`project_id` → `projectid`).
fn normalized(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A reference to a secret rather than the secret (`$FIREBASE_TOKEN`, `${{ secrets.X }}`,
/// `%TOKEN%`), GitHub's mask `***`, or a placeholder.
fn is_reference(value: &str) -> bool {
    let v = value.trim();
    v.starts_with('$')
        || v.len() > 2 && v.starts_with('%') && v.ends_with('%')
        || v.starts_with('<') && v.ends_with('>')
        || !v.is_empty() && v.chars().all(|c| c == '*')
}

/// Loopback and unspecified addresses, and Docker's names for the host: they say nothing
/// about the network the machine is on.
pub fn local_host(host: &str) -> bool {
    let h = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "0.0.0.0"
        || h == "::"
        || h == "::1"
        || h == "0:0:0:0:0:0:0:1"
        || h == "host.docker.internal"
        || h == "gateway.docker.internal"
        || h.strip_prefix("127.").is_some_and(|rest| {
            rest.split('.').count() == 3
                && rest
                    .split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A character of a project ID or a name, as far as finding it as a whole word goes.
fn is_id(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn is_host(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'.'
}

/// A character inside a path's names (not a separator); non-ASCII bytes count as one.
fn is_path_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~' | b'+' | b'@') || b >= 0x80
}

/// Rewrites `s`: at each character `f` may match some bytes and give their replacement.
fn rewrite(s: &str, mut f: impl FnMut(&str, usize) -> Option<(usize, String)>) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut i, mut copied) = (0, 0);
    while i < s.len() {
        if s.is_char_boundary(i)
            && let Some((len, with)) = f(s, i)
            && len > 0
        {
            out.push_str(&s[copied..i]);
            out.push_str(&with);
            i += len;
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// `s[i..]` starts with `prefix`, ignoring ASCII case and treating `\` as `/`.
fn starts_with_path(s: &str, i: usize, prefix: &str) -> bool {
    let (s, p) = (s.as_bytes(), prefix.as_bytes());
    s.len() >= i + p.len()
        && s[i..i + p.len()].iter().zip(p).all(|(&a, &b)| {
            let a = if a == b'\\' { b'/' } else { a };
            a.eq_ignore_ascii_case(&b)
        })
}

fn starts_with_ignore_case(s: &str, i: usize, prefix: &str) -> bool {
    s.as_bytes()
        .get(i..i + prefix.len())
        .is_some_and(|b| b.eq_ignore_ascii_case(prefix.as_bytes()))
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been replaced since the redactor was made, or since [`Redactor::reset_counts`].
    pub fn counts(&self) -> &BTreeMap<Kind, usize> {
        &self.counts
    }

    /// Forgets the counts, not what it knows.
    pub fn reset_counts(&mut self) {
        self.counts.clear();
    }

    fn count(&mut self, kind: Kind) {
        *self.counts.entry(kind).or_default() += 1;
    }

    /// An absolute path to replace with the placeholder of `kind` (`ProjectPath`, `HomePath`
    /// or `TempPath`). A root directory (`/`, `C:\`) is refused: it would match every path.
    pub fn path(&mut self, path: &str, kind: Kind) {
        let normal = path.replace('\\', "/");
        let normal = normal.trim_end_matches('/');
        let named = normal.trim_start_matches('/');
        let drive_only = named.len() <= 2 && named.ends_with(':');
        if named.is_empty() || drive_only || !(normal.starts_with('/') || normal.contains(":/")) {
            return;
        }
        if !self
            .paths
            .iter()
            .any(|(p, _)| p.eq_ignore_ascii_case(normal))
        {
            self.paths.push((normal.to_owned(), kind));
            self.paths.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        }
    }

    /// A value to replace wherever it appears, such as a secret environment variable's. Short
    /// values are refused: they would match ordinary words.
    pub fn secret(&mut self, value: &str) {
        let value = value.trim();
        if value.len() >= 8 && !is_reference(value) && !self.secrets.iter().any(|s| s == value) {
            self.secrets.push(value.to_owned());
            self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        }
    }

    /// The machine's name (`HostName`) or the user's (`UserName`), replaced where it is a
    /// whole word. Generic names (`root`, `runner`) identify no one and are kept. For a host
    /// name, its first label is taken as well (`alices-laptop` of `alices-laptop.local`).
    pub fn name(&mut self, name: &str, kind: Kind) {
        let name = name.trim().to_ascii_lowercase();
        let mut candidates = vec![name.clone()];
        if kind == Kind::HostName
            && let Some((first, _)) = name.split_once('.')
        {
            candidates.push(first.to_owned());
        }
        for candidate in candidates {
            if candidate.len() >= 3
                && !GENERIC_NAMES.contains(&candidate.as_str())
                && !candidate.bytes().all(|b| b.is_ascii_digit())
                && !self.names.iter().any(|(n, _)| *n == candidate)
            {
                self.names.push((candidate, kind));
                self.names.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
            }
        }
    }

    /// A `.firebaserc` alias, which `--project` may name instead of an ID. A name that is
    /// also a project ID stays one.
    pub fn alias(&mut self, alias: &str) {
        if !self.aliases.iter().any(|a| a == alias) && !self.projects.iter().any(|p| p == alias) {
            self.aliases.push(alias.to_owned());
        }
    }

    /// A project ID's placeholder (`<project-id-2>`), or the ID itself when it is a `demo-`
    /// one, an alias or a reference. A project number becomes `<project-number>`.
    pub fn project(&mut self, id: &str) -> String {
        let id = id.trim();
        let known = self.projects.iter().position(|p| p == id);
        if known.is_none()
            && (id.is_empty()
                || id.starts_with("demo-")
                || is_reference(id)
                || self.aliases.iter().any(|a| a == id))
        {
            return id.to_owned();
        }
        self.count(Kind::ProjectId);
        if id.bytes().all(|b| b.is_ascii_digit()) {
            return "<project-number>".to_owned();
        }
        let n = match known {
            Some(i) => i + 1,
            None => {
                self.projects.push(id.to_owned());
                self.projects.len()
            }
        };
        format!("<project-id-{n}>")
    }

    /// A host: kept when it is a loopback one, `<host>` otherwise (and replaced wherever it
    /// appears from then on).
    pub fn host(&mut self, host: &str) -> String {
        let trimmed = host.trim();
        if trimmed.is_empty() || local_host(trimmed) || is_reference(trimmed) {
            return host.to_owned();
        }
        let key = trimmed
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        if !self.hosts.contains(&key) {
            self.hosts.push(key);
            self.hosts.sort_by_key(|h| std::cmp::Reverse(h.len()));
        }
        self.count(Kind::Host);
        "<host>".to_owned()
    }

    /// `host:port`, `[v6]:port`, a bare host, or a URL (`STORAGE_EMULATOR_HOST` holds
    /// `http://host:port`): the host as [`Redactor::host`] decides, the port as it is.
    pub fn host_port(&mut self, value: &str) -> String {
        if let Some(at) = value.find("://") {
            let rest = &value[at + 3..];
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let authority = &rest[..end];
            let (credentials, address) = match authority.rfind('@') {
                Some(i) => {
                    self.count(Kind::UrlCredentials);
                    ("<credentials>@", &authority[i + 1..])
                }
                None => ("", authority),
            };
            return format!(
                "{}{credentials}{}{}",
                &value[..at + 3],
                self.host_port(address),
                self.text(&rest[end..])
            );
        }
        let (host, port) = if let Some(rest) = value.strip_prefix('[') {
            match rest.split_once(']') {
                Some((host, port)) => (format!("[{host}]"), port.to_owned()),
                None => (value.to_owned(), String::new()),
            }
        } else {
            match value.rsplit_once(':') {
                Some((host, port)) if !host.contains(':') => (host.to_owned(), format!(":{port}")),
                _ => (value.to_owned(), String::new()),
            }
        };
        let port =
            if port.is_empty() || port.len() > 1 && port[1..].bytes().all(|b| b.is_ascii_digit()) {
                port
            } else {
                self.text(&port)
            };
        format!("{}{port}", self.host(&host))
    }

    /// A value read under `key` (a JSON key, an environment variable's name): the whole
    /// value when the key is secret, the project or host rules when it names one, and
    /// [`Redactor::text`] otherwise.
    pub fn value(&mut self, key: &str, value: &str) -> String {
        if value.is_empty() {
            return String::new();
        }
        if secret_key(key) {
            if is_reference(value) || matches!(value, "true" | "false" | "null") {
                return value.to_owned();
            }
            self.count(Kind::Secret);
            return "<redacted>".to_owned();
        }
        match normalized(key).as_str() {
            "projectid"
            | "project"
            | "projectnumber"
            | "messagingsenderid"
            | "site"
            | "gcloudproject"
            | "googlecloudproject"
            | "cloudsdkcoreproject" => {
                return self.project(value);
            }
            "host" => return self.host(value),
            k if k.ends_with("emulatorhost") => return self.host_port(value),
            _ => {}
        }
        self.text(value)
    }

    /// A JSON value read under `key`: strings as [`Redactor::value`] decides, every scalar
    /// under a secret key, keys through [`Redactor::text`]. In an object that pairs a `key`
    /// (or `name`) with a `value`, as firebase.json's hosting headers do, the value is secret
    /// when the name is.
    pub fn json(&mut self, key: &str, value: &Value) -> Value {
        if secret_key(key) && !value.is_string() {
            return self.hide(value);
        }
        match value {
            Value::String(s) => Value::String(self.value(key, s)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.json(key, v)).collect()),
            Value::Object(map) => {
                // {"key": "Authorization", "value": "…"}: the name is a name, the value
                // secret when the name is.
                let pair = map.contains_key("value");
                let named_secret = pair
                    && ["key", "name"]
                        .iter()
                        .any(|k| map.get(*k).and_then(Value::as_str).is_some_and(secret_key));
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    let v = match v {
                        _ if named_secret && k == "value" => self.hide(v),
                        Value::String(s) if pair && (k == "key" || k == "name") => {
                            Value::String(self.text(s))
                        }
                        _ => self.json(k, v),
                    };
                    out.insert(self.text(k), v);
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }

    /// Every scalar of `value` as `<redacted>` (booleans and nulls kept).
    fn hide(&mut self, value: &Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.iter().map(|v| self.hide(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (self.text(k), self.hide(v)))
                    .collect(),
            ),
            Value::Null | Value::Bool(_) => value.clone(),
            _ => {
                self.count(Kind::Secret);
                Value::String("<redacted>".to_owned())
            }
        }
    }

    /// Free text: every rule, in an order where none undoes another.
    pub fn text(&mut self, text: &str) -> String {
        let s = self.private_keys(text);
        let s = self.known_secrets(&s);
        let s = self.urls(&s);
        let s = self.tokens(&s);
        let s = self.assignments(&s);
        let s = self.project_flags(&s);
        let s = self.firebase_hosts(&s);
        let s = self.app_ids(&s);
        let s = self.paths(&s);
        let s = self.known_words(&s);
        self.emails(&s)
    }

    fn private_keys(&mut self, s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("-----BEGIN ") {
            let after = &rest[start + 11..];
            let label = after.find("-----").map(|end| &after[..end]);
            match label {
                Some(label) if label.contains("PRIVATE KEY") && !label.contains('\n') => {
                    out.push_str(&rest[..start]);
                    out.push_str("<private-key>");
                    self.count(Kind::PrivateKey);
                    let end = format!("-----END {label}-----");
                    rest = match rest[start..].find(&end) {
                        Some(e) => &rest[start + e + end.len()..],
                        // Unterminated: everything after it may be the key.
                        None => "",
                    };
                }
                _ => {
                    out.push_str(&rest[..start + 11]);
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn known_secrets(&mut self, s: &str) -> String {
        let mut out = s.to_owned();
        let mut found = 0;
        for secret in &self.secrets {
            let n = out.matches(secret.as_str()).count();
            if n > 0 {
                found += n;
                out = out.replace(secret.as_str(), "<redacted>");
            }
        }
        for _ in 0..found {
            self.count(Kind::Secret);
        }
        out
    }

    fn urls(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            let scheme_char = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-');
            if !b[i].is_ascii_alphabetic() || i > 0 && scheme_char(b[i - 1]) {
                return None;
            }
            let mut j = i;
            while j < b.len() && scheme_char(b[j]) {
                j += 1;
            }
            if !s[j..].starts_with("://") {
                return None;
            }
            let start = j + 3;
            let mut end = start;
            while end < b.len()
                && !b[end].is_ascii_whitespace()
                && !matches!(b[end], b'"' | b'\'' | b'<' | b'>' | b'`' | b'|')
            {
                end += 1;
            }
            while end > start
                && matches!(
                    b[end - 1],
                    b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']'
                )
            {
                end -= 1;
            }
            Some((end - i, self.url(&s[i..end], j - i)))
        })
    }

    /// One URL: its credentials and its secret query parameters.
    fn url(&mut self, url: &str, scheme: usize) -> String {
        let rest = &url[scheme + 3..];
        let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..auth_end];
        let mut out = url[..scheme + 3].to_owned();
        match authority.rfind('@') {
            Some(at)
                if at > 0 && &authority[..at] != "git" && !authority[..at].starts_with('<') =>
            {
                self.count(Kind::UrlCredentials);
                out.push_str("<credentials>");
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        let tail = &rest[auth_end..];
        let Some(q) = tail.find('?') else {
            out.push_str(tail);
            return out;
        };
        out.push_str(&tail[..=q]);
        let query = &tail[q + 1..];
        let (query, fragment) = query.split_at(query.find('#').unwrap_or(query.len()));
        let params: Vec<String> = query
            .split('&')
            .map(|param| match param.split_once('=') {
                Some((name, value))
                    if !value.is_empty() && !is_reference(value) && secret_param(name) =>
                {
                    self.count(Kind::Secret);
                    format!("{name}=<redacted>")
                }
                _ => param.to_owned(),
            })
            .collect();
        out.push_str(&params.join("&"));
        out.push_str(fragment);
        out
    }

    fn tokens(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            if i > 0 && is_word(b[i - 1]) {
                return None;
            }
            let tail = |from: usize, extra: &[u8]| {
                b[from..]
                    .iter()
                    .take_while(|&&c| {
                        c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || extra.contains(&c)
                    })
                    .count()
            };
            for (prefix, min, kind) in TOKENS {
                if s[i..].starts_with(prefix) {
                    let extra: &[u8] = if prefix == "ya29." || prefix == "SG." {
                        b"."
                    } else {
                        b""
                    };
                    let n = tail(i + prefix.len(), extra);
                    if n >= min {
                        self.count(kind);
                        let placeholder = if kind == Kind::ApiKey {
                            "<api-key>"
                        } else {
                            "<token>"
                        };
                        return Some((prefix.len() + n, placeholder.to_owned()));
                    }
                }
            }
            // A JWT: header.payload.signature, base64url, the header and payload JSON objects.
            if s[i..].starts_with("eyJ") {
                let header = tail(i, b"");
                let mut j = i + header;
                if header >= 10 && b.get(j) == Some(&b'.') {
                    let payload = tail(j + 1, b"");
                    if payload >= 4 {
                        j += 1 + payload;
                        if b.get(j) == Some(&b'.') {
                            j += 1 + tail(j + 1, b"");
                        }
                        self.count(Kind::Token);
                        return Some((j - i, "<token>".to_owned()));
                    }
                }
            }
            if starts_with_ignore_case(s, i, "bearer ") {
                let from = i + 7;
                let n = b[from..]
                    .iter()
                    .take_while(|&&c| c.is_ascii_alphanumeric() || b"-._~+/=".contains(&c))
                    .count();
                let token = &s[from..from + n];
                if n >= 8 && token != "owner" {
                    self.count(Kind::Token);
                    return Some((7 + n, format!("{}<redacted>", &s[i..from])));
                }
            }
            None
        })
    }

    /// `key=value`, `key: value`, `"key": "value"`, `--key value`, `--key=value` with a
    /// secret-looking key.
    fn assignments(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            let key_char = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
            if i > 0 && key_char(b[i - 1]) {
                return None;
            }
            let mut j = i;
            let dashes = b[j..].iter().take(2).take_while(|&&c| c == b'-').count();
            j += dashes;
            let quote = b.get(j).copied().filter(|c| matches!(c, b'"' | b'\''));
            if quote.is_some() {
                if dashes > 0 {
                    return None;
                }
                j += 1;
            }
            let key_start = j;
            if !b
                .get(j)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                return None;
            }
            while j < b.len() && key_char(b[j]) {
                j += 1;
            }
            let key = &s[key_start..j];
            if let Some(q) = quote {
                if b.get(j) != Some(&q) {
                    return None;
                }
                j += 1;
            }
            if !secret_key(key) {
                // Skip the key: a part of it is no key of its own.
                return Some((j - i, s[i..j].to_owned()));
            }
            // `'authorization: bearer x'`: the quote before the key ends the value.
            let enclosing =
                (quote.is_none() && i > 0 && matches!(b[i - 1], b'"' | b'\'')).then(|| b[i - 1]);
            let mut k = j;
            while k < b.len() && matches!(b[k], b' ' | b'\t') {
                k += 1;
            }
            let spaced = k > j;
            let separator = match b.get(k) {
                Some(b'=') => b'=',
                Some(b':') if !matches!(b.get(k + 1), Some(b':' | b'/')) => b':',
                _ if dashes == 2 && spaced => b' ',
                _ => return None,
            };
            // `KEY= cmd` sets KEY to nothing; `key = value` and `key: value` are spaced.
            if separator != b' ' {
                k += 1;
                if separator == b':' || spaced {
                    while k < b.len() && matches!(b[k], b' ' | b'\t') {
                        k += 1;
                    }
                }
            }
            let (value_end, inner) = match b.get(k) {
                None | Some(b'{' | b'[') => return None,
                Some(b'-') if separator == b' ' => return None,
                Some(&q @ (b'"' | b'\'')) => {
                    let mut e = k + 1;
                    while e < b.len() && b[e] != q && b[e] != b'\n' {
                        // An escaped character, whole.
                        e += if b[e] == b'\\' {
                            1 + s[e + 1..].chars().next().map_or(0, char::len_utf8)
                        } else {
                            1
                        };
                    }
                    let e = e.min(b.len());
                    let close = if b.get(e) == Some(&q) { e + 1 } else { e };
                    (close, (k + 1, e))
                }
                Some(_) => {
                    let mut e = k;
                    if separator == b':' {
                        while e < b.len()
                            && Some(b[e]) != enclosing
                            && !matches!(b[e], b'\n' | b'\r' | b',' | b';' | b'}' | b']')
                        {
                            e += 1;
                        }
                        while e > k && b[e - 1].is_ascii_whitespace() {
                            e -= 1;
                        }
                    } else {
                        while e < b.len()
                            && !b[e].is_ascii_whitespace()
                            && !matches!(
                                b[e],
                                b',' | b';' | b'&' | b'|' | b')' | b'}' | b']' | b'"' | b'\''
                            )
                        {
                            e += 1;
                        }
                    }
                    (e, (k, e))
                }
            };
            let value = &s[inner.0..inner.1];
            if value.is_empty()
                || is_reference(value)
                || matches!(value, "true" | "false" | "null")
                // The emulator's admin token, which every emulator user has.
                || value.eq_ignore_ascii_case("bearer owner")
            {
                return Some((value_end - i, s[i..value_end].to_owned()));
            }
            self.count(Kind::Secret);
            Some((
                value_end - i,
                format!("{}<redacted>{}", &s[i..inner.0], &s[inner.1..value_end]),
            ))
        })
    }

    /// `--project <id>`, `--project=<id>`, `-P <id>`, `--project_id <id>`, `projects/<id>`.
    fn project_flags(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            let boundary = i == 0 || !(is_id(b[i - 1]) || b[i - 1] == b'.');
            if !boundary {
                return None;
            }
            const FLAGS: [&str; 6] = [
                "--project",
                "--project_id",
                "--project-id",
                "--projectId",
                "-P",
                "projects/",
            ];
            let flag = FLAGS
                .iter()
                .filter(|f| s[i..].starts_with(**f))
                .max_by_key(|f| f.len())?;
            let mut j = i + flag.len();
            if *flag == "projects/" {
                let n = b[j..]
                    .iter()
                    .take_while(|&&c| c.is_ascii_alphanumeric() || c == b'-')
                    .count();
                let id = &s[j..j + n];
                if n < 2 || !id.as_bytes()[0].is_ascii_alphabetic() {
                    return None;
                }
                let with = self.project(id);
                return Some((flag.len() + n, format!("{flag}{with}")));
            }
            match b.get(j) {
                Some(b'=') => j += 1,
                Some(b' ' | b'\t') => {
                    while j < b.len() && matches!(b[j], b' ' | b'\t') {
                        j += 1;
                    }
                }
                _ => return None,
            }
            let quote = b.get(j).copied().filter(|c| matches!(c, b'"' | b'\''));
            let start = j + usize::from(quote.is_some());
            let n = b[start..]
                .iter()
                .take_while(|&&c| {
                    !c.is_ascii_whitespace()
                        && !matches!(c, b'"' | b'\'' | b';' | b'&' | b'|' | b')')
                })
                .count();
            let id = &s[start..start + n];
            if n == 0 || id.starts_with(['-', '{', '(', '*']) {
                return None;
            }
            let with = self.project(id);
            Some((start + n - i, format!("{}{with}", &s[i..start])))
        })
    }

    /// `<id>.firebaseapp.com` and the other hosts named after a project.
    fn firebase_hosts(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            if i > 0 && is_host(b[i - 1]) || !b[i].is_ascii_alphanumeric() {
                return None;
            }
            let n = b[i..].iter().take_while(|&&c| is_host(c)).count();
            let host = &s[i..i + n];
            let lower = host.to_ascii_lowercase();
            let Some(&(suffix, is_project)) = FIREBASE_HOSTS
                .iter()
                .find(|(suffix, _)| lower.ends_with(suffix) && lower.len() > suffix.len())
            else {
                return Some((n, host.to_owned()));
            };
            // The labels before the suffix: one (`<project>.web.app`), or a database and its
            // region (`<db>.europe-west1.firebasedatabase.app`).
            let prefix = &host[..host.len() - suffix.len()];
            let label_end = prefix.find('.').unwrap_or(prefix.len());
            let label = &prefix[..label_end];
            if label.starts_with("demo-") {
                return Some((n, host.to_owned()));
            }
            let labels = prefix.split('.').count();
            let single = labels == 1 || labels == 2 && suffix == ".firebasedatabase.app";
            let with = if is_project && single {
                let id = label.strip_suffix("-default-rtdb");
                let with = self.project(id.unwrap_or(label));
                format!(
                    "{with}{}{}",
                    if id.is_some() { "-default-rtdb" } else { "" },
                    &host[label_end..]
                )
            } else {
                // Not a label that is a project ID by itself: all of it goes.
                self.count(Kind::ProjectId);
                format!("<project-id>{suffix}")
            };
            Some((n, with))
        })
    }

    /// `1:1234567890:web:0a1b2c` and the Android and iOS ones.
    fn app_ids(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b':') {
                return None;
            }
            let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
            let first = digits(i);
            if first == 0 || b.get(i + first) != Some(&b':') {
                return None;
            }
            let j = i + first + 1;
            let number = digits(j);
            if number < 6 || b.get(j + number) != Some(&b':') {
                return None;
            }
            let k = j + number + 1;
            let platform = ["web:", "android:", "ios:"]
                .into_iter()
                .find(|p| s[k..].starts_with(p))?;
            let h = k + platform.len();
            let hex = b[h..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            if hex < 4 {
                return None;
            }
            self.count(Kind::AppId);
            Some((h + hex - i, "<app-id>".to_owned()))
        })
    }

    fn paths(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            let boundary = i == 0
                || !(is_path_name(b[i - 1]) || matches!(b[i - 1], b'/' | b'\\'))
                || s[..i].ends_with("file://");
            if !boundary {
                return None;
            }
            let ends = |end: usize| b.get(end).is_none_or(|&c| !is_path_name(c));
            let (len, kind) = self
                .paths
                .iter()
                .find(|(path, _)| starts_with_path(s, i, path) && ends(i + path.len()))
                .map(|(path, kind)| (path.len(), *kind))
                // Another user's home directory, or a temporary directory.
                .or_else(|| generic_home(s, i).map(|n| (n, Kind::HomePath)))
                .or_else(|| macos_temp(s, i).map(|n| (n, Kind::TempPath)))?;
            self.count(kind);
            Some((len, placeholder(kind).to_owned()))
        })
    }

    fn known_words(&mut self, s: &str) -> String {
        if self.projects.is_empty() && self.hosts.is_empty() && self.names.is_empty() {
            return s.to_owned();
        }
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            let before = if i == 0 { None } else { Some(b[i - 1]) };
            if before.is_some_and(|c| is_id(c) || c == b'<') {
                return None;
            }
            let after_ok = |end: usize, host: bool| {
                b.get(end).is_none_or(|&c| {
                    !(is_id(c)
                        || c == b'>'
                        || host && c == b'.' && b.get(end + 1).is_some_and(|d| is_host(*d)))
                })
            };
            let project = self
                .projects
                .iter()
                .enumerate()
                .find(|(_, id)| s[i..].starts_with(id.as_str()) && after_ok(i + id.len(), false))
                .map(|(n, id)| (id.len(), Kind::ProjectId, format!("<project-id-{}>", n + 1)));
            let host = || {
                self.hosts
                    .iter()
                    .find(|h| {
                        before != Some(b'.')
                            && starts_with_ignore_case(s, i, h)
                            && after_ok(i + h.len(), true)
                    })
                    .map(|h| (h.len(), Kind::Host, "<host>".to_owned()))
            };
            let name = || {
                self.names
                    .iter()
                    .find(|(n, _)| starts_with_ignore_case(s, i, n) && after_ok(i + n.len(), false))
                    .map(|(n, kind)| (n.len(), *kind, placeholder(*kind).to_owned()))
            };
            let (len, kind, with) = project.or_else(host).or_else(name)?;
            self.count(kind);
            Some((len, with))
        })
    }

    fn emails(&mut self, s: &str) -> String {
        rewrite(s, |s, i| {
            let b = s.as_bytes();
            let local_char =
                |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-');
            if i > 0 && local_char(b[i - 1]) || !local_char(b[i]) {
                return None;
            }
            let local = b[i..].iter().take_while(|&&c| local_char(c)).count();
            let at = i + local;
            if b.get(at) != Some(&b'@') || &s[i..at] == "git" {
                return Some((local, s[i..at].to_owned()));
            }
            let mut end = at + 1 + b[at + 1..].iter().take_while(|&&c| is_host(c)).count();
            while end > at + 1 && matches!(b[end - 1], b'.' | b'-') {
                end -= 1;
            }
            let domain = &s[at + 1..end];
            let tld = domain.rsplit('.').next().unwrap_or("");
            if !domain.contains('.')
                || tld.len() < 2
                || !tld.bytes().all(|c| c.is_ascii_alphabetic())
            {
                return Some((local, s[i..at].to_owned()));
            }
            self.count(Kind::Email);
            Some((end - i, "<email>".to_owned()))
        })
    }
}

fn placeholder(kind: Kind) -> &'static str {
    match kind {
        Kind::ProjectPath => "<project>",
        Kind::HomePath => "<home>",
        Kind::TempPath => "<temp>",
        Kind::HostName => "<hostname>",
        Kind::UserName => "<user>",
        Kind::Host => "<host>",
        Kind::Email => "<email>",
        Kind::ApiKey => "<api-key>",
        Kind::Token => "<token>",
        Kind::PrivateKey => "<private-key>",
        Kind::UrlCredentials => "<credentials>",
        Kind::AppId => "<app-id>",
        Kind::ProjectId => "<project-id>",
        Kind::Secret => "<redacted>",
    }
}

/// A query parameter that carries a secret in a URL.
fn secret_param(name: &str) -> bool {
    secret_key(name)
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "code" | "auth" | "x-goog-credential" | "x-amz-credential" | "x-amz-security-token"
        )
}

/// The length of a home directory at `i` that is not a registered one: `/Users/<name>`,
/// `/home/<name>`, `/var/home/<name>`, `/root`, `C:\Users\<name>`. `/Users/Shared` and
/// Homebrew's `/home/linuxbrew` belong to no one.
fn generic_home(s: &str, i: usize) -> Option<usize> {
    let b = s.as_bytes();
    let drive = b.get(i).is_some_and(u8::is_ascii_alphabetic) && b.get(i + 1) == Some(&b':');
    let start = if drive { i + 2 } else { i };
    let prefix = ["/users/", "/home/", "/var/home/", "/media/"]
        .into_iter()
        .find(|p| starts_with_path(s, start, p));
    let Some(prefix) = prefix else {
        if !drive
            && starts_with_path(s, i, "/root")
            && b.get(i + 5).is_none_or(|&c| !is_path_name(c))
        {
            return Some(5);
        }
        return None;
    };
    if drive && prefix != "/users/" {
        return None;
    }
    let name_start = start + prefix.len();
    let name = b[name_start..]
        .iter()
        .take_while(|&&c| is_path_name(c) || c == b' ' && drive)
        .count();
    let name_text = s[name_start..name_start + name].trim_end();
    if name_text.is_empty()
        || ["shared", "linuxbrew", "public", "default", "all users"]
            .contains(&name_text.to_ascii_lowercase().as_str())
    {
        return None;
    }
    Some(name_start + name_text.len() - i)
}

/// The length of a macOS per-user temporary directory at `i`:
/// `/var/folders/<xx>/<id>/T` (or `/private/var/folders/…`, and the `C` and `X` siblings).
fn macos_temp(s: &str, i: usize) -> Option<usize> {
    let b = s.as_bytes();
    let start = ["/private/var/folders/", "/var/folders/"]
        .into_iter()
        .find(|p| starts_with_path(s, i, p))?
        .len();
    let mut j = i + start;
    for _ in 0..2 {
        let n = b[j..].iter().take_while(|&&c| is_path_name(c)).count();
        if n == 0 {
            return None;
        }
        j += n;
        if b.get(j) == Some(&b'/') {
            j += 1;
        } else {
            return Some(j - i);
        }
    }
    if matches!(b.get(j), Some(b'T' | b'C' | b'X'))
        && b.get(j + 1).is_none_or(|&c| !is_path_name(c))
    {
        return Some(j + 1 - i);
    }
    Some(j - 1 - i)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A fake secret: `prefix` and `n` more characters, put together at run time so that no
    /// secret scanner takes this file for a leak.
    fn fake(prefix: &str, n: usize) -> String {
        let tail: String = "x7Qk2Lm9Zp4Rt8Vw3Ny6".chars().cycle().take(n).collect();
        format!("{prefix}{tail}")
    }

    fn redact(text: &str) -> String {
        Redactor::new().text(text)
    }

    /// Asserts that each input redacts to its expected output.
    fn cases(r: &mut Redactor, cases: &[(&str, &str)]) {
        for (input, expected) in cases {
            assert_eq!(&r.text(input), expected, "input: {input}");
        }
    }

    #[test]
    fn private_key_blocks() {
        let begin = format!("-----BEGIN {} KEY-----", "PRIVATE");
        let end = format!("-----END {} KEY-----", "PRIVATE");
        let pem = format!("before\n{begin}\nMIIEvQIBADANBgkq\nhkiG9w0BAQEFAASC\n{end}\nafter");
        assert_eq!(redact(&pem), "before\n<private-key>\nafter");
        // As a JSON string, in a service account file (whose key is secret as well).
        let escaped = format!(r#"{{"private_key": "{begin}\nMIIEvQ\n{end}\n"}}"#);
        assert_eq!(redact(&escaped), r#"{"private_key": "<redacted>"}"#);
        let other = format!(r#"{{"pem": "{begin}\nMIIEvQ\n{end}\n"}}"#);
        assert_eq!(redact(&other), r#"{"pem": "<private-key>\n"}"#);
        // RSA, EC, OpenSSH and PGP keys too; unterminated: the rest goes.
        let rsa = format!("-----BEGIN RSA {} KEY-----\nAAAA", "PRIVATE");
        assert_eq!(redact(&format!("x {rsa}")), "x <private-key>");
        let pgp = format!(
            "-----BEGIN PGP {} KEY BLOCK-----\nx\n-----END PGP {} KEY BLOCK-----",
            "PRIVATE", "PRIVATE"
        );
        assert_eq!(redact(&pgp), "<private-key>");
        // A certificate is public.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(redact(cert), cert);
    }

    #[test]
    fn known_secrets_go_wherever_they_are() {
        let mut r = Redactor::new();
        let token = fake("tok", 24);
        r.secret(&token);
        r.secret("short");
        r.secret("$FIREBASE_TOKEN");
        assert_eq!(r.text(&format!("a{token}b")), "a<redacted>b");
        assert_eq!(
            r.text(&format!("echo {token} {token}")),
            "echo <redacted> <redacted>"
        );
        // Too short to be told from a word, and a reference, are not secrets.
        assert_eq!(r.text("short $FIREBASE_TOKEN"), "short $FIREBASE_TOKEN");
        assert_eq!(r.counts()[&Kind::Secret], 3);
    }

    #[test]
    fn credentials_in_urls() {
        let mut r = Redactor::new();
        cases(
            &mut r,
            &[
                (
                    "https://user:hunter2@example.com/x",
                    "https://<credentials>@example.com/x",
                ),
                (
                    "clone https://ghtoken123@github.com/org/repo.git",
                    "clone https://<credentials>@github.com/org/repo.git",
                ),
                (
                    "postgres://app:p%40ss@db.internal:5432/x",
                    "postgres://<credentials>@db.internal:5432/x",
                ),
                (
                    "redis://:secretpw@cache:6379",
                    "redis://<credentials>@cache:6379",
                ),
                // ssh's `git@` is no credential; an @ after the authority is no user.
                (
                    "git+ssh://git@github.com/org/repo.git",
                    "git+ssh://git@github.com/org/repo.git",
                ),
                (
                    "https://example.com/@someone",
                    "https://example.com/@someone",
                ),
                (
                    "(see https://user:pw@example.com).",
                    "(see https://<credentials>@example.com).",
                ),
            ],
        );
        assert_eq!(r.counts()[&Kind::UrlCredentials], 5);
    }

    #[test]
    fn secret_query_parameters() {
        let key = fake("AIza", 35);
        cases(
            &mut Redactor::new(),
            &[
                (
                    "https://example.com/api?alt=json&access_token=abc123&page=2",
                    "https://example.com/api?alt=json&access_token=<redacted>&page=2",
                ),
                // The fragment after a secret goes with it.
                (
                    "https://storage.googleapis.com/b/o?X-Goog-Credential=sa%2F2026&X-Goog-Signature=0f1e#frag",
                    "https://storage.googleapis.com/b/o?X-Goog-Credential=<redacted>&X-Goog-Signature=<redacted>",
                ),
                (
                    "https://example.com/a?page=2#top",
                    "https://example.com/a?page=2#top",
                ),
                (
                    &format!("https://firestore.googleapis.com/v1/x?key={key}"),
                    "https://firestore.googleapis.com/v1/x?key=<redacted>",
                ),
                (
                    "https://example.com/cb?code=4%2F0Ab&state=xyz",
                    "https://example.com/cb?code=<redacted>&state=xyz",
                ),
                // Empty values and references stay.
                (
                    "https://example.com/?token=&key=$KEY",
                    "https://example.com/?token=&key=$KEY",
                ),
            ],
        );
    }

    #[test]
    fn token_shapes() {
        let mut r = Redactor::new();
        for (prefix, n, placeholder) in [
            ("AIza", 35, "<api-key>"),
            ("ya29.", 60, "<token>"),
            ("1//0g", 40, "<token>"),
            ("ghp_", 36, "<token>"),
            ("github_pat_", 60, "<token>"),
            ("xoxb-", 40, "<token>"),
            ("sk_live_", 24, "<token>"),
            ("AKIA", 16, "<token>"),
            ("npm_", 36, "<token>"),
            ("GOCSPX-", 28, "<token>"),
        ] {
            let token = fake(prefix, n);
            assert_eq!(
                r.text(&format!("x {token} y")),
                format!("x {placeholder} y"),
                "{prefix}"
            );
            assert_eq!(
                r.text(&format!("\"{token}\",")),
                format!("\"{placeholder}\","),
                "{prefix}"
            );
        }
        assert_eq!(r.counts()[&Kind::ApiKey], 2);
        assert_eq!(r.counts()[&Kind::Token], 18);
        // Too short, or inside a word: not those tokens.
        cases(
            &mut r,
            &[
                ("AIza alone", "AIza alone"),
                ("ghp_short", "ghp_short"),
                ("task-runner sk-learn", "task-runner sk-learn"),
                ("1//comment", "1//comment"),
            ],
        );
    }

    #[test]
    fn jwts_and_bearer_tokens() {
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            fake("hb", 30),
            fake("zd", 80),
            fake("Sf", 43)
        );
        // The emulator's mock tokens are unsigned: the signature is empty.
        let mock = format!("eyJ{}.eyJ{}.", fake("hb", 20), fake("zd", 60));
        cases(
            &mut Redactor::new(),
            &[
                (&format!("token {jwt} end"), "token <token> end"),
                (&format!("\"{mock}\""), "\"<token>\""),
                (
                    &format!("Authorization: Bearer {}", fake("ab", 30)),
                    "Authorization: <redacted>",
                ),
                (
                    &format!("curl -H 'authorization: bearer {}'", fake("ab", 30)),
                    "curl -H 'authorization: <redacted>'",
                ),
                (&format!("bearer {}", fake("ab", 30)), "bearer <redacted>"),
                // The emulator's admin token, and words.
                ("Authorization: Bearer owner", "Authorization: Bearer owner"),
                ("a bearer token", "a bearer token"),
                ("eyJ is how JSON starts", "eyJ is how JSON starts"),
            ],
        );
    }

    #[test]
    fn secret_looking_keys() {
        for key in [
            "FIREBASE_TOKEN",
            "GITHUB_TOKEN",
            "apiKey",
            "api_key",
            "x-api-key",
            "client_secret",
            "clientSecret",
            "privateKeyId",
            "private_key",
            "DB_PASSWORD",
            "passwd",
            "accessToken",
            "Authorization",
            "SESSION_COOKIE",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "secrets",
            "key",
            "webhookUrl",
            "sentry_dsn",
        ] {
            assert!(secret_key(key), "{key} is secret");
        }
        for key in [
            "host",
            "port",
            "projectId",
            "rules",
            "indexes",
            "monkey",
            "keyboard",
            "auth",
            "authDomain",
            "FIRESTORE_EMULATOR_HOST",
            "lockfile",
            "source",
            "predeploy",
        ] {
            assert!(!secret_key(key), "{key} is not secret");
        }
        assert_eq!(words("privateKeyId"), ["private", "key", "id"]);
        assert_eq!(words("HTTPServer_v2"), ["http", "server", "v2"]);
    }

    #[test]
    fn secret_assignments() {
        cases(
            &mut Redactor::new(),
            &[
                (
                    "FIREBASE_TOKEN=abc123 firebase deploy",
                    "FIREBASE_TOKEN=<redacted> firebase deploy",
                ),
                (
                    "export DB_PASSWORD='hunter 2'",
                    "export DB_PASSWORD='<redacted>'",
                ),
                (
                    "firebase deploy --token abc123 --only hosting",
                    "firebase deploy --token <redacted> --only hosting",
                ),
                (
                    "firebase deploy --token=abc123",
                    "firebase deploy --token=<redacted>",
                ),
                (
                    r#"{"apiKey": "abc123", "authDomain": "x"}"#,
                    r#"{"apiKey": "<redacted>", "authDomain": "x"}"#,
                ),
                (
                    r#"{"password": "a \"quoted\" pass", "user": "u"}"#,
                    r#"{"password": "<redacted>", "user": "u"}"#,
                ),
                (
                    "password: correct horse battery\nnext: line",
                    "password: <redacted>\nnext: line",
                ),
                ("client_secret=abc&x=1", "client_secret=<redacted>&x=1"),
                // References, masks, empty values and literals stay.
                (
                    "firebase deploy --token \"$FIREBASE_TOKEN\"",
                    "firebase deploy --token \"$FIREBASE_TOKEN\"",
                ),
                (
                    "--token ${{ secrets.FIREBASE_TOKEN }}",
                    "--token ${{ secrets.FIREBASE_TOKEN }}",
                ),
                ("FIREBASE_TOKEN=***", "FIREBASE_TOKEN=***"),
                ("TOKEN= next", "TOKEN= next"),
                (r#""token": null"#, r#""token": null"#),
                // Not a value: another flag, a path, a nested object, a Rust path.
                ("--token --only x", "--token --only x"),
                (
                    "cat /run/secrets/db_password",
                    "cat /run/secrets/db_password",
                ),
                (
                    r#"{"credentials": {"type": "x"}}"#,
                    r#"{"credentials": {"type": "x"}}"#,
                ),
                ("secret::Secret", "secret::Secret"),
                (
                    "monkey=banana keyboard: qwerty",
                    "monkey=banana keyboard: qwerty",
                ),
            ],
        );
    }

    #[test]
    fn project_ids_after_flags_and_in_paths() {
        let mut r = Redactor::new();
        cases(
            &mut r,
            &[
                (
                    "firebase emulators:exec --project acme-prod-4821 'npm test'",
                    "firebase emulators:exec --project <project-id-1> 'npm test'",
                ),
                (
                    "firebase use --project=acme-staging",
                    "firebase use --project=<project-id-2>",
                ),
                (
                    "firebase deploy -P acme-prod-4821",
                    "firebase deploy -P <project-id-1>",
                ),
                (
                    "hidane --project_id \"acme-prod-4821\"",
                    "hidane --project_id \"<project-id-1>\"",
                ),
                (
                    "POST /v1/projects/acme-prod-4821/databases/(default)/documents:commit",
                    "POST /v1/projects/<project-id-1>/databases/(default)/documents:commit",
                ),
                // Seen once, replaced everywhere.
                (
                    "acme-prod-4821 and acme-staging",
                    "<project-id-1> and <project-id-2>",
                ),
                // A longer name that merely contains it is another name.
                ("acme-prod-4821-backup", "acme-prod-4821-backup"),
                // demo- projects, references and templates stay.
                (
                    "--project demo-test projects/demo-test/databases",
                    "--project demo-test projects/demo-test/databases",
                ),
                (
                    "--project $PROJECT --project=${{ vars.P }}",
                    "--project $PROJECT --project=${{ vars.P }}",
                ),
                (
                    "projects/{project}/databases/{database}",
                    "projects/{project}/databases/{database}",
                ),
                ("projects/-/x", "projects/-/x"),
                // Not those flags.
                (
                    "--projects 3 -Path x myprojects/acme",
                    "--projects 3 -Path x myprojects/acme",
                ),
            ],
        );
        assert_eq!(r.project("acme-prod-4821"), "<project-id-1>");
        assert_eq!(r.project("123456789012"), "<project-number>");
        r.alias("staging");
        assert_eq!(
            r.text("firebase use staging --project staging"),
            "firebase use staging --project staging"
        );
        // An alias that is also a project ID stays an ID.
        r.alias("acme-staging");
        assert_eq!(r.project("acme-staging"), "<project-id-2>");
    }

    #[test]
    fn hosts_named_after_projects() {
        let mut r = Redactor::new();
        cases(
            &mut r,
            &[
                (
                    "acme-prod.firebaseapp.com",
                    "<project-id-1>.firebaseapp.com",
                ),
                (
                    "https://acme-prod.web.app/x",
                    "https://<project-id-1>.web.app/x",
                ),
                (
                    "gs://acme-prod.appspot.com/a.png",
                    "gs://<project-id-1>.appspot.com/a.png",
                ),
                (
                    "https://acme-prod-default-rtdb.firebaseio.com",
                    "https://<project-id-1>-default-rtdb.firebaseio.com",
                ),
                (
                    "acme-db.europe-west1.firebasedatabase.app",
                    "<project-id-2>.europe-west1.firebasedatabase.app",
                ),
                (
                    "acme-prod.firebasestorage.app",
                    "<project-id-1>.firebasestorage.app",
                ),
                (
                    "https://us-central1-acme-prod.cloudfunctions.net/api",
                    "https://<project-id>.cloudfunctions.net/api",
                ),
                (
                    "https://api-x7k2-uc.a.run.app",
                    "https://<project-id>.run.app",
                ),
                ("www.acme.web.app", "<project-id>.web.app"),
                // demo- and bare suffixes stay.
                (
                    "demo-test.web.app firebaseapp.com",
                    "demo-test.web.app firebaseapp.com",
                ),
                ("example.com", "example.com"),
            ],
        );
    }

    #[test]
    fn firebase_app_ids() {
        cases(
            &mut Redactor::new(),
            &[
                (
                    "appId: 1:123456789012:web:0a1b2c3d4e5f6a7b",
                    "appId: <app-id>",
                ),
                ("1:123456789012:android:0a1b2c3d4e5f", "<app-id>"),
                ("1:123456789012:ios:0a1b2c3d", "<app-id>"),
                ("1:2:3 and 10:30:00", "1:2:3 and 10:30:00"),
            ],
        );
    }

    #[test]
    fn project_and_home_paths() {
        let mut r = Redactor::new();
        r.path("/Users/alice/work/acme-app", Kind::ProjectPath);
        r.path("/Users/alice/", Kind::HomePath);
        r.path("/", Kind::HomePath);
        r.path("relative/dir", Kind::HomePath);
        cases(
            &mut r,
            &[
                (
                    "/Users/alice/work/acme-app/firebase.json",
                    "<project>/firebase.json",
                ),
                ("cd /Users/alice/work/acme-app && ls", "cd <project> && ls"),
                (
                    "/Users/alice/.cache/firebase/emulators",
                    "<home>/.cache/firebase/emulators",
                ),
                ("file:///Users/alice/x.txt", "file://<home>/x.txt"),
                ("PATH=/Users/alice/bin:/usr/bin", "PATH=<home>/bin:/usr/bin"),
                ("\"/Users/ALICE/Work/acme-app\"", "\"<project>\""),
                // Another directory that starts like one of them is not it, but still a home.
                (
                    "/Users/alice/work/acme-application/x",
                    "<home>/work/acme-application/x",
                ),
                ("/Users/alicebob/x", "<home>/x"),
                // `/` and relative paths were refused: system paths and relative ones stay.
                (
                    "/usr/lib/jvm/java-21/bin/java",
                    "/usr/lib/jvm/java-21/bin/java",
                ),
                (
                    ".github/workflows/ci.yml functions/src/index.ts",
                    ".github/workflows/ci.yml functions/src/index.ts",
                ),
                ("~/.cache/firebase", "~/.cache/firebase"),
            ],
        );
    }

    #[test]
    fn windows_paths() {
        let mut r = Redactor::new();
        r.path(r"C:\Users\Alice Smith", Kind::HomePath);
        r.path(r"C:\", Kind::HomePath);
        r.path(r"D:\src\acme-app", Kind::ProjectPath);
        cases(
            &mut r,
            &[
                (
                    r"C:\Users\Alice Smith\AppData\Local\Temp\x",
                    r"<home>\AppData\Local\Temp\x",
                ),
                ("c:/users/alice smith/.cache", "<home>/.cache"),
                (r"d:\SRC\acme-app\firebase.json", r"<project>\firebase.json"),
                (r"C:\Users\bob\proj", r"<home>\proj"),
                (r"C:\Program Files\Java\bin", r"C:\Program Files\Java\bin"),
                (r"C:\Users\Public\x", r"C:\Users\Public\x"),
            ],
        );
    }

    #[test]
    fn other_homes_and_temporary_directories() {
        cases(
            &mut Redactor::new(),
            &[
                (
                    "/home/runner/work/app/app/firebase.json",
                    "<home>/work/app/app/firebase.json",
                ),
                ("/Users/bob/Library/Caches", "<home>/Library/Caches"),
                ("/var/home/carol/x /media/dave/USB", "<home>/x <home>/USB"),
                ("/root/.config", "<home>/.config"),
                (
                    "/var/folders/ab/cd12ef34/T/hidane-shim-1/java",
                    "<temp>/hidane-shim-1/java",
                ),
                ("/private/var/folders/ab/cd12ef34/T/x", "<temp>/x"),
                // Shared and Homebrew's directories belong to no one.
                (
                    "/Users/Shared/x /home/linuxbrew/.linuxbrew/bin/firebase",
                    "/Users/Shared/x /home/linuxbrew/.linuxbrew/bin/firebase",
                ),
                ("/rootfs/x /homework", "/rootfs/x /homework"),
            ],
        );
    }

    #[test]
    fn machine_and_user_names() {
        let mut r = Redactor::new();
        r.name("Alices-Laptop.local", Kind::HostName);
        r.name("alice", Kind::UserName);
        // Generic and short names identify no one.
        r.name("runner", Kind::UserName);
        r.name("ab", Kind::UserName);
        r.name("localhost", Kind::HostName);
        cases(
            &mut r,
            &[
                ("ssh alice@alices-laptop.local", "ssh <user>@<hostname>"),
                (
                    "Listening on ALICES-LAPTOP:8080",
                    "Listening on <hostname>:8080",
                ),
                ("by Alice, alice's", "by <user>, <user>'s"),
                ("malice alice-wonder alice_b", "malice alice-wonder alice_b"),
                ("runner ab localhost", "runner ab localhost"),
            ],
        );
    }

    #[test]
    fn hosts_other_than_loopback() {
        let mut r = Redactor::new();
        for local in [
            "localhost",
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "[::1]",
            "0.0.0.0",
            "::",
            "host.docker.internal",
        ] {
            assert_eq!(r.host(local), local);
        }
        assert_eq!(r.host("10.20.30.40"), "<host>");
        assert_eq!(r.host("firestore.corp.example"), "<host>");
        assert_eq!(r.host_port("127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(r.host_port("10.20.30.40:8081"), "<host>:8081");
        assert_eq!(r.host_port("[fd00::1]:8080"), "<host>:8080");
        assert_eq!(r.host_port("[::1]:8080"), "[::1]:8080");
        assert_eq!(r.host_port("fd00::2"), "<host>");
        assert_eq!(r.host_port("http://10.0.0.2:9199"), "http://<host>:9199");
        assert_eq!(
            r.host_port("http://u:p@localhost:9199/x"),
            "http://<credentials>@localhost:9199/x"
        );
        assert_eq!(r.host_port("$HOST:8080"), "$HOST:8080");
        assert!(!local_host("127.0.0.1.nip.io"));
        // Once seen, replaced wherever it appears.
        cases(
            &mut r,
            &[
                (
                    "FIRESTORE_EMULATOR_HOST=10.20.30.40:8081",
                    "FIRESTORE_EMULATOR_HOST=<host>:8081",
                ),
                ("http://FIRESTORE.corp.example/x", "http://<host>/x"),
                (
                    "10.20.30.400 110.20.30.40 a.firestore.corp.example",
                    "10.20.30.400 110.20.30.40 a.firestore.corp.example",
                ),
            ],
        );
    }

    #[test]
    fn email_addresses() {
        cases(
            &mut Redactor::new(),
            &[
                ("alice@example.com", "<email>"),
                (
                    "\"Alice\" <alice.smith+ci@mail.example.co.jp>,",
                    "\"Alice\" <<email>>,",
                ),
                ("sa@acme-prod.iam.gserviceaccount.com", "<email>"),
                ("mailto:bob@example.org.", "mailto:<email>."),
                // Versions, scopes, actions and ssh are not addresses.
                (
                    "firebase-tools@15.33.0 @google-cloud/firestore",
                    "firebase-tools@15.33.0 @google-cloud/firestore",
                ),
                (
                    "actions/checkout@v4 hidane-dev/setup-hidane@v1",
                    "actions/checkout@v4 hidane-dev/setup-hidane@v1",
                ),
                (
                    "npm:pkg@^1.2.3 pkg@latest a@b",
                    "npm:pkg@^1.2.3 pkg@latest a@b",
                ),
                ("git@github.com:org/repo.git", "git@github.com:org/repo.git"),
            ],
        );
    }

    #[test]
    fn values_by_key() {
        let mut r = Redactor::new();
        assert_eq!(r.value("apiKey", "anything"), "<redacted>");
        assert_eq!(
            r.value("FIREBASE_TOKEN", "$FIREBASE_TOKEN"),
            "$FIREBASE_TOKEN"
        );
        assert_eq!(r.value("secure_token", "true"), "true");
        assert_eq!(r.value("projectId", "acme-prod-4821"), "<project-id-1>");
        assert_eq!(r.value("project_id", "demo-test"), "demo-test");
        assert_eq!(
            r.value("messagingSenderId", "123456789012"),
            "<project-number>"
        );
        assert_eq!(r.value("site", "acme-site"), "<project-id-2>");
        assert_eq!(r.value("host", "10.0.0.9"), "<host>");
        assert_eq!(r.value("emulator_host", "10.0.0.9:8080"), "<host>:8080");
        assert_eq!(r.value("source", "functions"), "functions");
        assert_eq!(
            r.value("rules", "/Users/bob/firestore.rules"),
            "<home>/firestore.rules"
        );
    }

    #[test]
    fn json_values() {
        let mut r = Redactor::new();
        let input = json!({
            "hosting": {
                "site": "acme-site",
                "headers": [{"source": "**", "headers": [
                    {"key": "Authorization", "value": "Basic YWxhZGRpbjpvcGVu"},
                    {"key": "Cache-Control", "value": "max-age=60"}
                ]}],
                "rewrites": [{"source": "/api", "function": "api"}]
            },
            "credentials": {"user": "u", "pin": 1234, "nested": ["a", {"b": "c"}], "enabled": true},
            "apiKeys": [1, 2],
            "emulators": {"firestore": {"host": "10.1.2.3", "port": 8080}},
            "contact": {"alice@example.com": "owner"}
        });
        assert_eq!(
            r.json("", &input),
            json!({
                "hosting": {
                    "site": "<project-id-1>",
                    "headers": [{"source": "**", "headers": [
                        {"key": "Authorization", "value": "<redacted>"},
                        {"key": "Cache-Control", "value": "max-age=60"}
                    ]}],
                    "rewrites": [{"source": "/api", "function": "api"}]
                },
                "credentials": {"user": "<redacted>", "pin": "<redacted>", "nested": ["<redacted>", {"b": "<redacted>"}], "enabled": true},
                "apiKeys": ["<redacted>", "<redacted>"],
                "emulators": {"firestore": {"host": "<host>", "port": 8080}},
                "contact": {"<email>": "owner"}
            })
        );
    }

    #[test]
    fn redacting_twice_changes_nothing_and_any_text_is_safe() {
        let mut r = Redactor::new();
        r.path("/Users/alice", Kind::HomePath);
        r.name("alice", Kind::UserName);
        r.secret(&fake("tok", 20));
        let corpus = [
            format!(
                "--project acme-x https://u:p@h.example/?token=t {} alice@example.com /Users/alice/x",
                fake("AIza", 35)
            ),
            "café naïve Zoë ✓ /Users/alice/Zoë/ü \"password\": \"ü\\\"é\" --token ✓".to_owned(),
            "\\\"--token \"unterminated".to_owned(),
            "-----BEGIN ".to_owned(),
            "eyJ.eyJ. projects/ --project= https:// @ : = \\".to_owned(),
            "C: C:\\ /var/folders/ /var/folders/ab".to_owned(),
        ];
        for text in &corpus {
            let once = r.text(text);
            assert_eq!(r.text(&once), once, "{text}");
        }
        // Every prefix of every text, cut at each character.
        for text in &corpus {
            for (i, _) in text.char_indices() {
                let _ = r.text(&text[..i]);
                let _ = r.text(&text[i..]);
            }
        }
    }

    #[test]
    fn counts_by_kind() {
        let mut r = Redactor::new();
        r.text("alice@example.com bob@example.com --project acme-x");
        assert_eq!(r.counts()[&Kind::Email], 2);
        assert_eq!(r.counts()[&Kind::ProjectId], 1);
        r.reset_counts();
        assert!(r.counts().is_empty());
        // What it learnt stays.
        assert_eq!(r.text("acme-x"), "<project-id-1>");
        assert_eq!(Kind::Email.noun(), ("e-mail address", "e-mail addresses"));
    }
}
