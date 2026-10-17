//! Datastore keys: how a request's keys are checked, with the official emulator's messages
//! (`tests/fixtures/datastore_entities.json`), and how hidane holds them.
//!
//! A key is a path of elements, each a kind and a numeric ID or a name, in a partition: a
//! project, a database and a namespace. The last element may be incomplete (no ID and no name)
//! where an ID is to be allocated.

use hidane_core::path::ResourcePath;
use hidane_proto::google::datastore::v1::{
    Key, PartitionId,
    key::{PathElement, path_element::IdType},
};
use tonic::Status;

/// The longest path.
const MAX_PATH: usize = 100;
/// The longest kind, name or namespace's text, in bytes.
const MAX_BYTES: usize = 1500;

/// A key path element's identifier. Numeric IDs come before names, as Datastore orders keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Id {
    Id(i64),
    Name(String),
}

/// A complete key path, below its namespace.
pub type Path = Vec<(String, Id)>;

/// Where a key lives within a database.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Located {
    pub namespace: String,
    pub path: Path,
}

/// The database a request names: its project and database ID (`""` for the default one).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Database {
    pub project: String,
    pub database: String,
}

impl Database {
    /// `projects/{p}/databases/{d}`, the default database as `(default)`, for the lock table.
    pub fn name(&self) -> String {
        let database = if self.database.is_empty() {
            "(default)"
        } else {
            &self.database
        };
        format!("projects/{}/databases/{database}", self.project)
    }

    /// The partition of `namespace` in this database, as responses print it.
    pub fn partition(&self, namespace: &str) -> PartitionId {
        PartitionId {
            project_id: self.project.clone(),
            database_id: self.database.clone(),
            namespace_id: namespace.to_owned(),
        }
    }

    /// The key of `located` in this database.
    pub fn key(&self, located: &Located) -> Key {
        Key {
            partition_id: Some(self.partition(&located.namespace)),
            path: located
                .path
                .iter()
                .map(|(kind, id)| PathElement {
                    kind: kind.clone(),
                    id_type: Some(match id {
                        Id::Id(n) => IdType::Id(*n),
                        Id::Name(name) => IdType::Name(name.clone()),
                    }),
                })
                .collect(),
        }
    }
}

/// A request's project, as the official emulator reads it before anything else
/// (`tests/fixtures/datastore_projects.json`): App Engine's app IDs, an optional domain before a
/// `:` (dots allowed there) and a display ID of up to 100 lowercase letters, digits and hyphens,
/// not starting with a hyphen; a partition before a `~` passes, to be refused as a project
/// ([`check_project_id`]).
pub fn check_app_id(project: &str) -> Result<(), Status> {
    if project.is_empty() {
        return Err(Status::invalid_argument("The app id is the empty string."));
    }
    let app = match project.split_once('~') {
        Some((partition, app)) if !partition.is_empty() && partition.bytes().all(id_byte) => {
            if app.is_empty() { None } else { Some(app) }
        }
        Some(_) => None,
        None => Some(project),
    };
    if app.is_some_and(valid_app_id) {
        Ok(())
    } else {
        Err(Status::invalid_argument(format!(
            "\"{project}\" is an invalid app id."
        )))
    }
}

/// A partitioned app ID (`dev~p`) is no project, in Datastore mode.
pub fn check_project_id(project: &str) -> Result<(), Status> {
    if project.contains('~') {
        return Err(Status::invalid_argument(format!(
            "\"{project}\" is an invalid project id."
        )));
    }
    Ok(())
}

fn id_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
}

/// `[domain:]display`, without a partition.
fn valid_app_id(app: &str) -> bool {
    let starts_well = |s: &str| s.bytes().next().is_some_and(|b| b != b'-' && b != b'.');
    let display_ok = |display: &str| {
        display.len() <= 100
            && display.bytes().all(id_byte)
            && (display.is_empty() || starts_well(display))
    };
    match app.split_once(':') {
        None => !app.is_empty() && display_ok(app),
        Some((domain, display)) => {
            !display.contains(':')
                && display_ok(display)
                && domain.bytes().all(|b| id_byte(b) || b == b'.')
                && (domain.is_empty() || starts_well(domain))
        }
    }
}

/// How strictly a key is read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Rules {
    /// A key that is written: reserved kinds and names are refused.
    Write,
    /// A key value: reserved kinds and names are allowed.
    Read,
    /// A key that is looked up: reserved kinds, names and namespaces are allowed.
    Lookup,
}

/// What a key's last element must be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Last {
    Complete,
    Incomplete,
    Either,
}

/// A checked key: its namespace and path, the last element possibly incomplete.
#[derive(Clone, Debug)]
pub struct Checked {
    pub namespace: String,
    pub path: Vec<(String, Option<Id>)>,
}

impl Checked {
    /// The complete key, or `None` when its last element is incomplete.
    pub fn complete(&self) -> Option<Located> {
        Some(Located {
            namespace: self.namespace.clone(),
            path: self
                .path
                .iter()
                .map(|(kind, id)| Some((kind.clone(), id.clone()?)))
                .collect::<Option<_>>()?,
        })
    }

    /// The key with `id` as its last element's identifier.
    pub fn with_id(&self, id: i64) -> Located {
        let mut path: Path = self
            .path
            .iter()
            .map(|(kind, id)| (kind.clone(), id.clone().unwrap_or(Id::Id(0))))
            .collect();
        if let Some(last) = path.last_mut() {
            last.1 = Id::Id(id);
        }
        Located {
            namespace: self.namespace.clone(),
            path,
        }
    }
}

/// `[Parent: p, Child: ]`, as the messages about completeness print a path.
fn shown(path: &[PathElement]) -> String {
    let elements: Vec<String> = path
        .iter()
        .map(|element| {
            let id = match &element.id_type {
                Some(IdType::Id(n)) => n.to_string(),
                Some(IdType::Name(name)) => name.clone(),
                None => String::new(),
            };
            format!("{}: {id}", element.kind)
        })
        .collect();
    format!("[{}]", elements.join(", "))
}

/// A name Datastore reserves: `__`, at least one character, `__` (`____` is not one).
pub(super) fn reserved(text: &str) -> bool {
    text.len() >= 5 && text.starts_with("__") && text.ends_with("__")
}

/// A namespace's text: letters, digits, `.`, `-` and `_`, at most 100 of them.
fn namespace_ok(namespace: &str) -> bool {
    namespace.len() <= 100
        && namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Checks `key`, of a request on `database`: its partition ([`check_partition`]), then the
/// rest ([`check_unpartitioned`]).
pub fn check(key: &Key, database: &Database, rules: Rules, last: Last) -> Result<Checked, Status> {
    check_partition(key, database)?;
    check_unpartitioned(key, rules, last)
}

/// `project#database`, `project` for the default database, as `mismatched databases` says.
fn partition_name(project: &str, database: &str) -> String {
    if database.is_empty() {
        format!("<unknown!>~{project}")
    } else {
        format!("<unknown!>~{project}#{database}")
    }
}

/// Whether `key` is in the request's database: a key that names a project or a database names
/// both of the request's (an empty project is not the request's).
pub fn check_partition(key: &Key, database: &Database) -> Result<(), Status> {
    let Some(partition) = &key.partition_id else {
        return Ok(());
    };
    let named = !partition.project_id.is_empty() || !partition.database_id.is_empty();
    if named
        && (partition.project_id != database.project || partition.database_id != database.database)
    {
        return Err(Status::invalid_argument(format!(
            "mismatched databases within request: {} vs. {}",
            partition_name(&database.project, &database.database),
            partition_name(&partition.project_id, &partition.database_id)
        )));
    }
    Ok(())
}

/// Checks `key` but its partition's project and database: its namespace, then its path.
pub fn check_unpartitioned(key: &Key, rules: Rules, last: Last) -> Result<Checked, Status> {
    let namespace = key
        .partition_id
        .as_ref()
        .map(|p| p.namespace_id.clone())
        .unwrap_or_default();
    if !namespace.is_empty() {
        if rules != Rules::Lookup && reserved(&namespace) {
            return Err(Status::invalid_argument(format!(
                "The namespace id \"{namespace}\" is reserved."
            )));
        }
        if !namespace_ok(&namespace) {
            return Err(Status::invalid_argument(format!(
                "Illegal string \"{namespace}\" in namespace id."
            )));
        }
    }
    if key.path.is_empty() {
        return Err(Status::invalid_argument("Key path is empty."));
    }
    if key.path.len() > MAX_PATH {
        return Err(Status::invalid_argument(format!(
            "Key path is too long. Cannot exceed {MAX_PATH} elements."
        )));
    }
    let mut path = Vec::with_capacity(key.path.len());
    for element in &key.path {
        let kind = &element.kind;
        if kind.is_empty() {
            return Err(Status::invalid_argument("The kind is the empty string."));
        }
        if rules == Rules::Write && reserved(kind) {
            return Err(Status::invalid_argument(format!(
                "The kind \"{kind}\" is reserved."
            )));
        }
        if kind.len() > MAX_BYTES {
            return Err(Status::invalid_argument(format!(
                "The kind is longer than {MAX_BYTES} bytes."
            )));
        }
        let id = match &element.id_type {
            None => None,
            Some(IdType::Id(0)) => {
                return Err(Status::invalid_argument(
                    "Key path id is invalid. Must not be zero.",
                ));
            }
            Some(IdType::Id(n)) => Some(Id::Id(*n)),
            Some(IdType::Name(name)) => {
                if name.is_empty() {
                    return Err(Status::invalid_argument(
                        "The key path element name is the empty string.",
                    ));
                }
                if rules == Rules::Write && reserved(name) {
                    return Err(Status::invalid_argument(format!(
                        "The key path element name \"{name}\" is reserved."
                    )));
                }
                if name.len() > MAX_BYTES {
                    return Err(Status::invalid_argument(format!(
                        "The key path element name is longer than {MAX_BYTES} bytes."
                    )));
                }
                Some(Id::Name(name.clone()))
            }
        };
        path.push((kind.clone(), id));
    }
    let incomplete_parent = path[..path.len() - 1].iter().any(|(_, id)| id.is_none());
    let incomplete_last = path.last().is_some_and(|(_, id)| id.is_none());
    if incomplete_parent || (last == Last::Complete && incomplete_last) {
        return Err(Status::invalid_argument(format!(
            "Key path element must not be incomplete: {}",
            shown(&key.path)
        )));
    }
    if last == Last::Incomplete && !incomplete_last {
        return Err(Status::invalid_argument(format!(
            "Key path element must not be complete: {}",
            shown(&key.path)
        )));
    }
    Ok(Checked { namespace, path })
}

/// A complete key as the official emulator prints one in protocol-buffer text, in "no entity to
/// update": `app: "p"`, the namespace, each element, then a database but the default one. IDs
/// from 2,000,000,000 up are written
/// in hexadecimal (`0x77359400`), and strings escaped as [`quoted`] says.
pub fn proto_text(database: &Database, located: &Located) -> String {
    let mut out = format!("app: {}\n", quoted(&database.project));
    if !located.namespace.is_empty() {
        out.push_str(&format!("name_space: {}\n", quoted(&located.namespace)));
    }
    out.push_str("path <\n");
    for (kind, id) in &located.path {
        out.push_str(&format!("  Element {{\n    type: {}\n", quoted(kind)));
        match id {
            Id::Id(n) if *n >= 2_000_000_000 => out.push_str(&format!("    id: {n:#x}\n")),
            Id::Id(n) => out.push_str(&format!("    id: {n}\n")),
            Id::Name(name) => out.push_str(&format!("    name: {}\n", quoted(name))),
        }
        out.push_str("  }\n");
    }
    out.push_str(">\n");
    if !database.database.is_empty() {
        out.push_str(&format!("database_id: {}\n", quoted(&database.database)));
    }
    out
}

/// A string in the official emulator's protocol-buffer text: in double quotes, `"` and `\`
/// escaped, `\b`, `\t`, `\n`, `\f` and `\r` as such, other characters below a space and
/// those from U+007F to U+00FF as three octal digits, from U+0100 up as `\u` and the four hex
/// digits of each UTF-16 unit.
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ' '..='~' => out.push(c),
            '\0'..='\u{ff}' => out.push_str(&format!("\\{:03o}", u32::from(c))),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// A complete key as the official emulator prints one in "entity already exists":
/// `EntityRef[partitionRef=p!namespace, path=/Kind/name/Kind/5L]`, numeric IDs as Java longs.
pub fn entity_ref(database: &Database, located: &Located) -> String {
    let mut path = String::new();
    for (kind, id) in &located.path {
        path.push('/');
        path.push_str(kind);
        path.push('/');
        match id {
            Id::Id(n) => path.push_str(&format!("{n}L")),
            Id::Name(name) => path.push_str(name),
        }
    }
    let mut partition = database.project.clone();
    if !located.namespace.is_empty() {
        partition.push('!');
        partition.push_str(&located.namespace);
    }
    format!("EntityRef[partitionRef={partition}, path={path}]")
}

/// The document path the transaction locks of a key are kept under: the namespace, then the
/// path, numeric IDs as `__id<n>__` (which no name can be, as names like it are reserved).
pub fn lock_path(located: &Located) -> ResourcePath {
    let mut segments = vec!["__namespace__".to_owned(), located.namespace.clone()];
    for (kind, id) in &located.path {
        segments.push(kind.clone());
        segments.push(match id {
            Id::Id(n) => format!("__id{n}__"),
            Id::Name(name) => name.clone(),
        });
    }
    ResourcePath::from_segments(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(kind: &str, id: Option<IdType>) -> PathElement {
        PathElement {
            kind: kind.into(),
            id_type: id,
        }
    }

    #[test]
    fn paths_are_shown_as_the_messages_show_them() {
        let path = [
            element("Parent", None),
            element("Child", Some(IdType::Id(5))),
            element("Leaf", Some(IdType::Name("a".into()))),
        ];
        assert_eq!(shown(&path), "[Parent: , Child: 5, Leaf: a]");
    }

    #[test]
    fn ids_come_before_names() {
        assert!(Id::Id(i64::MAX) < Id::Name(String::new()));
        assert!(Id::Id(-1) < Id::Id(1));
    }

    #[test]
    fn keys_print_as_protocol_buffer_text() {
        let database = Database {
            project: "p".into(),
            database: String::new(),
        };
        let located = Located {
            namespace: String::new(),
            path: vec![
                ("P".into(), Id::Id(3)),
                ("Task".into(), Id::Name("x".into())),
            ],
        };
        assert_eq!(
            proto_text(&database, &located),
            "app: \"p\"\npath <\n  Element {\n    type: \"P\"\n    id: 3\n  }\n  Element {\n    type: \"Task\"\n    name: \"x\"\n  }\n>\n"
        );
        assert_eq!(
            entity_ref(&database, &located),
            "EntityRef[partitionRef=p, path=/P/3L/Task/x]"
        );
    }
}
