//! `firebase.json` as firebase-tools 15 reads it: which emulators `firebase emulators:start`
//! starts (`filterEmulatorTargets` in `lib/emulator/controller.js`), where the Firestore
//! emulator listens, and what it is given (rules, edition).

use serde::Serialize;

use super::{
    files::{self, Tree},
    json,
};

/// The emulators firebase-tools starts for a project (`ALL_SERVICE_EMULATORS`, then
/// `extensions`), in its order.
pub const SERVICE_EMULATORS: [&str; 12] = [
    "apphosting",
    "auth",
    "functions",
    "firestore",
    "database",
    "hosting",
    "pubsub",
    "storage",
    "eventarc",
    "dataconnect",
    "tasks",
    "extensions",
];

/// Top-level keys firebase-tools keeps (`Config.MATERIALIZE_TARGETS`): a service emulator
/// with one of these keys starts without an `emulators.<name>` entry.
const PRODUCT_KEYS: [&str; 9] = [
    "database",
    "extensions",
    "firestore",
    "functions",
    "hosting",
    "storage",
    "dataconnect",
    "apphosting",
    "auth",
];

/// Emulators that firebase-tools runs on a JVM (`binary: "java"`, or Pub/Sub's launcher,
/// which runs `java`).
pub const JAVA_EMULATORS: [&str; 4] = ["database", "firestore", "pubsub", "storage"];

/// `DEFAULT_PORTS` in `lib/emulator/constants.js`.
fn default_port(name: &str) -> Option<u16> {
    Some(match name {
        "ui" => 4000,
        "hub" => 4400,
        "logging" => 4500,
        "hosting" => 5000,
        "functions" | "extensions" => 5001,
        "apphosting" => 5002,
        "firestore" => 8080,
        "pubsub" => 8085,
        "database" => 9000,
        "auth" => 9099,
        "storage" => 9199,
        "eventarc" => 9299,
        "dataconnect" => 9399,
        "tasks" => 9499,
        _ => return None,
    })
}

/// A `firebase.json`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Its path in the project.
    pub file: String,
    pub doc: Result<json::Node, json::Error>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Facts {
    pub file: String,
    /// Where the Firestore emulator listens, and what it is given.
    pub firestore: Option<Firestore>,
    /// The emulators `firebase emulators:start` starts without `--only`.
    pub emulators: Vec<Emulator>,
    /// `emulators.singleProjectMode` (true unless set to false).
    pub single_project_mode: bool,
    /// `emulators.ui.enabled` is not false.
    pub ui: bool,
    /// The sources of the functions (`functions.source`).
    pub functions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Firestore {
    pub host: String,
    pub port: u16,
    pub websocket_port: Option<u16>,
    /// `emulators.firestore.edition`, else `firestore.edition`.
    pub edition: Option<String>,
    /// The `firestore` entries (one per database).
    pub databases: Vec<Database>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Database {
    pub database: Option<String>,
    pub rules: Option<String>,
    pub indexes: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Emulator {
    pub name: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// Runs on a JVM under firebase-tools.
    pub java: bool,
}

/// Reads the `firebase.json` at `file`, if there is one.
pub fn read(tree: &dyn Tree, file: &str) -> Option<Config> {
    let text = tree.read(file)?;
    Some(Config {
        file: file.to_owned(),
        doc: json::parse_with_comments(&text),
    })
}

/// The `firebase.json` firebase-tools uses when run in `dir`: the nearest one in it or above
/// it, within the project.
pub fn find<'a>(configs: &'a [Config], dir: &str) -> Option<&'a Config> {
    let mut dir = dir.to_owned();
    loop {
        let file = files::join(&dir, "firebase.json")?;
        if let Some(config) = configs.iter().find(|c| c.file == file) {
            return Some(config);
        }
        if dir.is_empty() {
            return None;
        }
        dir = files::parent(&dir).to_owned();
    }
}

impl Config {
    pub fn dir(&self) -> &str {
        files::parent(&self.file)
    }

    fn root(&self) -> Option<&json::Node> {
        self.doc.as_ref().ok().filter(|d| d.is_object())
    }

    fn truthy(&self, path: &[&str]) -> bool {
        self.root()
            .and_then(|r| r.at(path))
            .is_some_and(|v| match &v.kind {
                json::Kind::Null => false,
                json::Kind::Bool(b) => *b,
                json::Kind::Number(n) => *n != 0.0,
                json::Kind::Str(s) => !s.text.is_empty(),
                _ => true,
            })
    }

    /// Whether `emulators:start` / `emulators:exec` starts `name` with this `--only`
    /// (`filterEmulatorTargets`).
    pub fn starts(&self, name: &str, only: Option<&[String]>) -> bool {
        self.targets(only).contains(&name)
    }

    /// The emulators started with this `--only`.
    pub fn targets(&self, only: Option<&[String]>) -> Vec<&'static str> {
        let mut targets: Vec<&'static str> = SERVICE_EMULATORS
            .into_iter()
            .filter(|e| {
                (PRODUCT_KEYS.contains(e) && self.truthy(&[e]))
                    || self.truthy(&["emulators", e])
                    // The legacy top-level `rules` is the Realtime Database's.
                    || *e == "database" && self.truthy(&["rules"])
            })
            .collect();
        if targets.contains(&"functions") && !targets.contains(&"extensions") {
            targets.push("extensions");
        }
        if let Some(only) = only {
            targets.retain(|t| only.iter().any(|o| o == t));
        }
        targets
    }

    pub fn facts(&self) -> Option<Facts> {
        let root = self.root()?;
        let emulator = |name: &str| -> Emulator {
            let config = root.at(&["emulators", name]);
            Emulator {
                name: name.to_owned(),
                host: config
                    .and_then(|c| c.get("host"))
                    .and_then(|h| h.as_str())
                    .map(str::to_owned),
                port: config
                    .and_then(|c| c.get("port"))
                    .and_then(port)
                    .or_else(|| default_port(name)),
                java: JAVA_EMULATORS.contains(&name),
            }
        };
        let emulators: Vec<Emulator> = self
            .targets(None)
            .into_iter()
            .filter(|e| *e != "firestore")
            .map(emulator)
            .collect();
        let firestore = self.starts("firestore", None).then(|| {
            let config = root.at(&["emulators", "firestore"]);
            let edition = config
                .and_then(|c| c.get("edition"))
                .or_else(|| root.at(&["firestore", "edition"]))
                .and_then(|e| e.as_str())
                .map(str::to_owned);
            let entry = |node: &json::Node| Database {
                database: node
                    .get("database")
                    .and_then(|d| d.as_str())
                    .map(str::to_owned),
                rules: node
                    .get("rules")
                    .and_then(|r| r.as_str())
                    .map(str::to_owned),
                indexes: node
                    .get("indexes")
                    .and_then(|i| i.as_str())
                    .map(str::to_owned),
            };
            let databases = match root.get("firestore") {
                Some(node) if node.is_object() => vec![entry(node)],
                Some(node) => node
                    .as_array()
                    .unwrap_or_default()
                    .iter()
                    .filter(|n| n.is_object())
                    .map(entry)
                    .collect(),
                None => Vec::new(),
            };
            Firestore {
                host: config
                    .and_then(|c| c.get("host"))
                    .and_then(|h| h.as_str())
                    .unwrap_or("localhost")
                    .to_owned(),
                port: config
                    .and_then(|c| c.get("port"))
                    .and_then(port)
                    .unwrap_or(8080),
                websocket_port: config.and_then(|c| c.get("websocketPort")).and_then(port),
                edition,
                databases,
            }
        });
        let functions = match root.get("functions") {
            Some(node) if node.is_object() => vec![source(node)],
            Some(node) => node
                .as_array()
                .unwrap_or_default()
                .iter()
                .filter(|n| n.is_object())
                .map(source)
                .collect(),
            None => Vec::new(),
        };
        Some(Facts {
            file: self.file.clone(),
            firestore,
            emulators,
            single_project_mode: root
                .at(&["emulators", "singleProjectMode"])
                .and_then(json::Node::as_bool)
                .unwrap_or(true),
            ui: root
                .at(&["emulators", "ui", "enabled"])
                .and_then(json::Node::as_bool)
                .unwrap_or(true),
            functions,
        })
    }
}

fn source(node: &json::Node) -> String {
    node.get("source")
        .and_then(|s| s.as_str())
        .unwrap_or("functions")
        .to_owned()
}

/// A port given as a number or as a string of digits.
fn port(node: &json::Node) -> Option<u16> {
    match &node.kind {
        json::Kind::Str(s) => s.text.parse().ok(),
        _ => node.as_u64().and_then(|p| u16::try_from(p).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config {
            file: "firebase.json".into(),
            doc: json::parse_with_comments(text),
        }
    }

    #[test]
    fn targets_follow_firebase_tools() {
        let c = config(
            r#"{
              // comments are allowed
              "firestore": {"rules": "firestore.rules"},
              "functions": {"source": "fns"},
              "emulators": {"auth": {"port": 9099}, "storage": {}, "firestore": {"port": "8081"}}
            }"#,
        );
        assert_eq!(
            c.targets(None),
            ["auth", "functions", "firestore", "storage", "extensions"]
        );
        let only = ["firestore".to_owned(), "auth".to_owned()];
        assert_eq!(c.targets(Some(&only)), ["auth", "firestore"]);
        let facts = c.facts().unwrap();
        assert_eq!(facts.firestore.as_ref().unwrap().port, 8081);
        assert_eq!(facts.functions, ["fns"]);
        assert!(facts.single_project_mode);
        assert_eq!(
            facts
                .emulators
                .iter()
                .map(|e| (e.name.as_str(), e.java))
                .collect::<Vec<_>>(),
            [
                ("auth", false),
                ("functions", false),
                ("storage", true),
                ("extensions", false)
            ]
        );
    }

    #[test]
    fn nearest_config() {
        let configs = vec![
            config("{}"),
            Config {
                file: "app/firebase.json".into(),
                doc: json::parse("{}"),
            },
        ];
        assert_eq!(find(&configs, "app/web").unwrap().file, "app/firebase.json");
        assert_eq!(find(&configs, "functions").unwrap().file, "firebase.json");
    }
}
