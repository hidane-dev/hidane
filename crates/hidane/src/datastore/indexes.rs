//! `--require_indexes` in Datastore mode (#81): the composite indexes an `index.yaml` defines
//! (<https://cloud.google.com/datastore/docs/tools/indexconfig>), the index each query needs,
//! and when the file is read, as the official emulator reads and checks them
//! (`tests/fixtures/datastore_indexes.json`).
//!
//! The index a query needs, for each disjunct of its filter in disjunctive normal form (a
//! disjunct with the same filters as an earlier one left out): the properties of its equalities
//! (`=` and `IN`, the key's too) in the filter's order, then its row's: its orders, its
//! `distinctOn` properties, its inequalities (by name, the key last), then the properties it
//! projects or aggregates (by name, without the key), each once and ascending unless ordered
//! otherwise, a last ascending key left out. An index the file defines loses a last ascending key
//! too. One property other than the key, without an ancestor, needs no index; anything else needs
//! that very index, directions included, the kind and the ancestor alike. A query without a kind
//! needs one the file cannot define.
//!
//! The file is read when a database is first used (and after a reset), then kept; while it
//! cannot be read, every request to that database fails, without a message.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use hidane_proto::google::datastore::v1::{
    Filter, PropertyFilter, Query, composite_filter, filter::FilterType, property_filter,
    property_order::Direction,
};
use saphyr_parser::{Event, Parser, ScalarStyle};
use tonic::Status;

use super::keys::Database;

/// What a query answers when the file lacks the index it needs.
pub const MISSING: &str =
    "The query requires a composite index that is not defined in your indexes file.";

const KEY: &str = "__key__";

/// A composite index: a kind, whether it holds the ancestors, its properties in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composite {
    kind: Option<String>,
    ancestor: bool,
    /// Each property's name and whether it is descending.
    properties: Vec<(String, bool)>,
}

/// Leaves out a last ascending key: every index ends with it.
fn trimmed(properties: &mut Vec<(String, bool)>) {
    if properties
        .last()
        .is_some_and(|(name, descending)| name == KEY && !descending)
    {
        properties.pop();
    }
}

impl Composite {
    /// The built-in indexes: one property other than the key, of a kind, without ancestors.
    fn built_in(&self) -> bool {
        self.kind.is_some()
            && !self.ancestor
            && self.properties.len() == 1
            && self.properties[0].0 != KEY
    }
}

/// `--require_indexes`, with the file `--index_file` names (if it does): what each database
/// read from it.
#[derive(Default)]
pub struct Enforcement {
    file: Option<PathBuf>,
    read: Mutex<HashMap<String, Arc<Vec<Composite>>>>,
}

impl Enforcement {
    pub fn new(file: Option<PathBuf>) -> Self {
        Self {
            file,
            read: Mutex::default(),
        }
    }

    /// The indexes `database` serves queries with, read from the file when it is first used.
    /// While the file cannot be read, every request fails as the official emulator fails it:
    /// `UNKNOWN` without a message.
    pub fn indexes(&self, database: &Database) -> Result<Arc<Vec<Composite>>, Status> {
        let name = database.name();
        let mut read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(indexes) = read.get(&name) {
            return Ok(indexes.clone());
        }
        let parsed = match &self.file {
            None => Err("--require_indexes is set without --index_file".to_owned()),
            Some(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read the index file {}: {e}", path.display()))
                .and_then(|text| {
                    parse(&text).map_err(|e| format!("the index file {}: {e}", path.display()))
                }),
        };
        match parsed {
            Ok(indexes) => {
                let indexes = Arc::new(indexes);
                read.insert(name, indexes.clone());
                Ok(indexes)
            }
            Err(message) => {
                eprintln!("WARNING: {message}; every Datastore request fails until it can be read");
                Err(Status::unknown(""))
            }
        }
    }

    /// Forgets what each database read (`POST /reset`): the file is read again.
    pub fn reset(&self) {
        self.read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// Whether `defined` has the indexes `query` needs, `aggregated` the properties its sums and
/// averages read.
pub fn check(query: &Query, aggregated: &[String], defined: &[Composite]) -> Result<(), Status> {
    let disjuncts = query.filter.as_ref().map_or_else(|| vec![Vec::new()], dnf);
    let mut seen: Vec<&Vec<&PropertyFilter>> = Vec::new();
    for disjunct in &disjuncts {
        if seen.iter().any(|earlier| same(earlier, disjunct)) {
            continue;
        }
        seen.push(disjunct);
        let needed = needed(query, disjunct, aggregated);
        if !needed.built_in() && !defined.contains(&needed) {
            return Err(Status::failed_precondition(MISSING));
        }
    }
    Ok(())
}

/// A filter in disjunctive normal form: its property filters, in order, per disjunct.
fn dnf(filter: &Filter) -> Vec<Vec<&PropertyFilter>> {
    match &filter.filter_type {
        Some(FilterType::CompositeFilter(composite))
            if composite.op == composite_filter::Operator::Or as i32 =>
        {
            composite.filters.iter().flat_map(dnf).collect()
        }
        Some(FilterType::CompositeFilter(composite)) => {
            composite
                .filters
                .iter()
                .fold(vec![Vec::new()], |conjunctions, child| {
                    let child = dnf(child);
                    conjunctions
                        .iter()
                        .flat_map(|left| {
                            child.iter().map(move |right| {
                                left.iter().chain(right).copied().collect::<Vec<_>>()
                            })
                        })
                        .collect()
                })
        }
        Some(FilterType::PropertyFilter(property)) => vec![vec![property]],
        None => vec![Vec::new()],
    }
}

/// Two disjuncts with the same filters, in any order.
fn same(a: &[&PropertyFilter], b: &[&PropertyFilter]) -> bool {
    a.iter().all(|f| b.contains(f)) && b.iter().all(|f| a.contains(f))
}

fn name(filter: &PropertyFilter) -> &str {
    filter.property.as_ref().map_or("", |p| p.name.as_str())
}

/// The index one disjunct of `query` needs.
fn needed(query: &Query, disjunct: &[&PropertyFilter], aggregated: &[String]) -> Composite {
    use property_filter::Operator as O;
    let op = |f: &&PropertyFilter| O::try_from(f.op).ok();
    let mut properties: Vec<(String, bool)> = disjunct
        .iter()
        .filter(|f| matches!(op(f), Some(O::Equal | O::In)))
        .map(|f| (name(f).to_owned(), false))
        .collect();
    let mut inequalities: Vec<&str> = disjunct
        .iter()
        .filter(|f| {
            matches!(
                op(f),
                Some(
                    O::LessThan
                        | O::LessThanOrEqual
                        | O::GreaterThan
                        | O::GreaterThanOrEqual
                        | O::NotEqual
                        | O::NotIn
                )
            )
        })
        .map(|f| name(f))
        .collect();
    inequalities.sort_by(|a, b| (*a == KEY).cmp(&(*b == KEY)).then_with(|| a.cmp(b)));
    let mut row: Vec<(String, bool)> = query
        .order
        .iter()
        .map(|order| {
            let name = order.property.as_ref().map_or("", |p| p.name.as_str());
            (
                name.to_owned(),
                order.direction == Direction::Descending as i32,
            )
        })
        .collect();
    let mut read: Vec<&str> = query
        .projection
        .iter()
        .map(|p| p.property.as_ref().map_or("", |p| p.name.as_str()))
        .chain(aggregated.iter().map(String::as_str))
        .filter(|name| *name != KEY)
        .collect();
    read.sort_unstable();
    let distinct = query.distinct_on.iter().map(|p| p.name.as_str());
    for name in distinct.chain(inequalities).chain(read) {
        if !row.iter().any(|(n, _)| n == name) {
            row.push((name.to_owned(), false));
        }
    }
    trimmed(&mut row);
    properties.extend(row);
    Composite {
        kind: query.kind.first().map(|k| k.name.clone()),
        ancestor: disjunct.iter().any(|f| op(f) == Some(O::HasAncestor)),
        properties,
    }
}

/// A node of the file, each scalar as written (`plain`: unquoted).
#[derive(Debug)]
enum Node {
    Scalar { text: String, plain: bool },
    Sequence(Vec<Node>),
    Mapping(Vec<(String, Node)>),
}

impl Node {
    /// A plain empty scalar, `~` or `null`: YAML's null.
    fn is_null(&self) -> bool {
        matches!(self, Node::Scalar { text, plain: true }
            if matches!(text.as_str(), "" | "~" | "null" | "Null" | "NULL"))
    }
}

/// The first document of `text`, if it has one.
fn document(text: &str) -> Result<Option<Node>, String> {
    let mut events = Parser::new_from_str(text).map(|event| {
        event
            .map(|(event, _)| event)
            .map_err(|e| format!("not YAML: {e}"))
    });
    loop {
        match events.next().transpose()? {
            Some(Event::DocumentStart(_)) => return node(&mut events, None).map(Some),
            Some(Event::StreamEnd) | None => return Ok(None),
            Some(_) => {}
        }
    }
}

fn node<'a>(
    events: &mut impl Iterator<Item = Result<Event<'a>, String>>,
    first: Option<Event<'a>>,
) -> Result<Node, String> {
    let event = match first {
        Some(event) => event,
        None => events.next().ok_or("the file ends too soon")??,
    };
    match event {
        Event::Scalar(text, style, _, _) => Ok(Node::Scalar {
            text: text.into_owned(),
            plain: style == ScalarStyle::Plain,
        }),
        Event::SequenceStart(_, _) => {
            let mut items = Vec::new();
            loop {
                match events.next().ok_or("the file ends too soon")?? {
                    Event::SequenceEnd => return Ok(Node::Sequence(items)),
                    event => items.push(node(events, Some(event))?),
                }
            }
        }
        Event::MappingStart(_, _) => {
            let mut entries = Vec::new();
            loop {
                match events.next().ok_or("the file ends too soon")?? {
                    Event::MappingEnd => return Ok(Node::Mapping(entries)),
                    event => {
                        let Node::Scalar { text, .. } = node(events, Some(event))? else {
                            return Err("a key that is not a scalar".into());
                        };
                        entries.push((text, node(events, None)?));
                    }
                }
            }
        }
        Event::Alias(_) => Err("aliases are not supported".into()),
        _ => Err("unexpected YAML".into()),
    }
}

/// A mapping's entries as named fields: the last of each name, any other name refused.
fn fields<'a>(
    node: &'a Node,
    what: &str,
    names: &[&str],
) -> Result<HashMap<&'a str, &'a Node>, String> {
    let Node::Mapping(entries) = node else {
        return Err(format!("{what} is not a mapping"));
    };
    let mut out = HashMap::new();
    for (key, value) in entries {
        if !names.contains(&key.as_str()) {
            return Err(format!("unknown field '{key}' in {what}"));
        }
        out.insert(key.as_str(), value);
    }
    Ok(out)
}

/// A scalar that must be there: not null, not a collection.
fn scalar<'a>(node: Option<&'a Node>, what: &str) -> Result<&'a str, String> {
    match node {
        Some(node @ Node::Scalar { text, .. }) if !node.is_null() => Ok(text),
        _ => Err(format!("{what} is missing")),
    }
}

/// A sequence, or nothing (null or absent).
fn items<'a>(node: Option<&'a Node>, what: &str) -> Result<&'a [Node], String> {
    match node {
        None => Ok(&[]),
        Some(node) if node.is_null() => Ok(&[]),
        Some(Node::Sequence(items)) => Ok(items),
        Some(_) => Err(format!("{what} is not a sequence")),
    }
}

/// The indexes an `index.yaml` defines, or why it cannot be read. `ancestor` takes YAML 1.1's
/// booleans (`yes`, `no`, `on`, `off`, `y`, `n`, `true`, `false`, capitalised or in capitals);
/// `direction` takes `asc` or `desc`.
pub fn parse(text: &str) -> Result<Vec<Composite>, String> {
    let Some(root) = document(text)? else {
        return Err(
            "empty index configuration; it should at least have an empty 'indexes:' block".into(),
        );
    };
    let root = fields(&root, "the file", &["indexes"])?;
    let mut indexes = Vec::new();
    for index in items(root.get("indexes").copied(), "indexes")? {
        let index = fields(index, "an index", &["kind", "ancestor", "properties"])?;
        let kind = scalar(index.get("kind").copied(), "an index's kind")?.to_owned();
        let ancestor = match index.get("ancestor") {
            None => false,
            Some(value) => match scalar(Some(value), "ancestor")? {
                "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On"
                | "ON" => true,
                "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off"
                | "OFF" => false,
                other => return Err(format!("ancestor is not a boolean: {other}")),
            },
        };
        let mut properties = Vec::new();
        for property in items(index.get("properties").copied(), "properties")? {
            let property = fields(property, "a property", &["name", "direction"])?;
            let name = match property.get("name") {
                Some(Node::Scalar { text, plain: false }) => text.as_str(),
                other => scalar(other.copied(), "a property's name")?,
            };
            let descending = match property.get("direction") {
                None => false,
                Some(value) => match scalar(Some(value), "direction")? {
                    "asc" => false,
                    "desc" => true,
                    other => return Err(format!("direction is neither asc nor desc: {other}")),
                },
            };
            properties.push((name.to_owned(), descending));
        }
        trimmed(&mut properties);
        indexes.push(Composite {
            kind: Some(kind),
            ancestor,
            properties,
        });
    }
    Ok(indexes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_index_file() {
        let indexes = parse(
            "indexes:\n- kind: P\n  ancestor: yes\n  properties:\n  - name: a\n  - name: b\n    direction: desc\n  - name: __key__\n",
        )
        .unwrap();
        assert_eq!(
            indexes,
            [Composite {
                kind: Some("P".into()),
                ancestor: true,
                properties: vec![("a".into(), false), ("b".into(), true)],
            }]
        );
        assert_eq!(parse("indexes:\n").unwrap(), []);
        assert!(parse("").is_err());
        assert!(parse("indexes:\n- kind: P\n  ancestor: maybe\n").is_err());
    }
}
