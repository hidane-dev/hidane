//! Datastore queries (#81), as the official emulator in Datastore mode answers them
//! (`tests/fixtures/datastore_queries.json`, from `tools/oracle/datastore_queries.py`).
//!
//! A query reads an index, not entities: each entity has an index value for each indexed value
//! of a property (each element of an array, the properties of an entity value under dotted
//! names; not a value excluded from indexes, an entity value itself or an empty array), and
//! values of every type are ordered together ([`Indexed`]). The properties a query orders by,
//! has an inequality on, projects or is distinct on are its *row* properties: an entity gives
//! one result per combination of its values of them, so an entity with an array can come back
//! more than once. A filter on another property is true when any of the entity's values for it
//! matches. Results come in the order of the query's orders, then of its inequality,
//! `distinctOn` and projected properties, then of their keys.

use std::cmp::Ordering;

use hidane_proto::google::datastore::v1::{
    Entity, EntityResult, Filter, Key, PropertyFilter, Query, QueryResultBatch, Value,
    composite_filter, entity_result::ResultType, filter::FilterType, property_filter,
    property_order::Direction, query_result_batch::MoreResultsType, value::ValueType,
};
use hidane_proto::ordered::Properties;
use serde_json::{Value as Json, json};
use tonic::Status;

use super::keys::{Database, Id, Located};

/// An index value. The variants are in Datastore's order of types: null, integers and
/// timestamps (as microseconds) together, booleans, strings and blobs together (as bytes),
/// doubles (NaN last), geo points, keys.
#[derive(Clone, Debug)]
pub enum Indexed {
    Null,
    Int(i64),
    Bool(bool),
    Bytes(Vec<u8>),
    Double(f64),
    Geo(f64, f64),
    /// A key: its project, namespace and path.
    Key(String, String, Vec<(String, Id)>),
}

impl Indexed {
    fn rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Int(_) => 1,
            Self::Bool(_) => 2,
            Self::Bytes(_) => 3,
            Self::Double(_) => 4,
            Self::Geo(..) => 5,
            Self::Key(..) => 6,
        }
    }

    /// The value of a single (not array, not entity) Datastore value, or `None` for an entity
    /// value.
    pub fn of(value: &Value) -> Option<Self> {
        Some(match value.value_type.as_ref()? {
            ValueType::NullValue(_) => Self::Null,
            ValueType::BooleanValue(b) => Self::Bool(*b),
            ValueType::IntegerValue(n) => Self::Int(*n),
            ValueType::DoubleValue(d) => Self::Double(*d),
            ValueType::TimestampValue(ts) => {
                Self::Int(ts.seconds * 1_000_000 + i64::from(ts.nanos) / 1_000)
            }
            ValueType::StringValue(s) => Self::Bytes(s.as_bytes().to_vec()),
            ValueType::BlobValue(b) => Self::Bytes(b.clone()),
            ValueType::GeoPointValue(point) => Self::Geo(point.latitude, point.longitude),
            ValueType::KeyValue(key) => {
                let partition = key.partition_id.clone().unwrap_or_default();
                let (project, namespace) = (partition.project_id, partition.namespace_id);
                let path = key
                    .path
                    .iter()
                    .map(|e| {
                        let id = match &e.id_type {
                            Some(hidane_proto::google::datastore::v1::key::path_element::IdType::Id(n)) => Id::Id(*n),
                            Some(hidane_proto::google::datastore::v1::key::path_element::IdType::Name(n)) => Id::Name(n.clone()),
                            None => Id::Id(0),
                        };
                        (e.kind.clone(), id)
                    })
                    .collect();
                Self::Key(project, namespace, path)
            }
            ValueType::EntityValue(_) | ValueType::ArrayValue(_) => return None,
        })
    }

    /// The value a projection returns: the index value, marked with meaning 18.
    fn projected(&self, database: &Database) -> Value {
        let value_type = match self {
            Self::Null => ValueType::NullValue(0),
            Self::Int(n) => ValueType::IntegerValue(*n),
            Self::Bool(b) => ValueType::BooleanValue(*b),
            Self::Bytes(b) => ValueType::StringValue(String::from_utf8_lossy(b).into_owned()),
            Self::Double(d) => ValueType::DoubleValue(*d),
            Self::Geo(latitude, longitude) => {
                ValueType::GeoPointValue(hidane_proto::google::r#type::LatLng {
                    latitude: *latitude,
                    longitude: *longitude,
                })
            }
            Self::Key(project, namespace, path) => {
                let mut key = database.key(&Located {
                    namespace: namespace.clone(),
                    path: path.clone(),
                });
                if let Some(partition) = &mut key.partition_id {
                    partition.project_id.clone_from(project);
                }
                ValueType::KeyValue(key)
            }
        };
        Value {
            meaning: INDEX_VALUE,
            exclude_from_indexes: false,
            value_type: Some(value_type),
        }
    }

    fn to_json(&self) -> Json {
        match self {
            Self::Null => json!(["n"]),
            Self::Int(n) => json!(["i", n]),
            Self::Bool(b) => json!(["b", b]),
            Self::Bytes(b) => json!(["s", b]),
            Self::Double(d) => json!(["d", d.to_bits()]),
            Self::Geo(a, b) => json!(["g", a.to_bits(), b.to_bits()]),
            Self::Key(project, ns, path) => json!(["k", project, ns, path_json(path)]),
        }
    }

    fn from_json(json: &Json) -> Option<Self> {
        let parts = json.as_array()?;
        Some(match parts.first()?.as_str()? {
            "n" => Self::Null,
            "i" => Self::Int(parts.get(1)?.as_i64()?),
            "b" => Self::Bool(parts.get(1)?.as_bool()?),
            "s" => Self::Bytes(serde_json::from_value(parts.get(1)?.clone()).ok()?),
            "d" => Self::Double(f64::from_bits(parts.get(1)?.as_u64()?)),
            "g" => Self::Geo(
                f64::from_bits(parts.get(1)?.as_u64()?),
                f64::from_bits(parts.get(2)?.as_u64()?),
            ),
            "k" => Self::Key(
                parts.get(1)?.as_str()?.to_owned(),
                parts.get(2)?.as_str()?.to_owned(),
                path_from_json(parts.get(3)?)?,
            ),
            _ => return None,
        })
    }
}

fn path_json(path: &[(String, Id)]) -> Json {
    Json::Array(
        path.iter()
            .map(|(kind, id)| match id {
                Id::Id(n) => json!([kind, n]),
                Id::Name(name) => json!([kind, "", name]),
            })
            .collect(),
    )
}

fn path_from_json(json: &Json) -> Option<Vec<(String, Id)>> {
    json.as_array()?
        .iter()
        .map(|e| {
            let e = e.as_array()?;
            let kind = e.first()?.as_str()?.to_owned();
            let id = match e.len() {
                2 => Id::Id(e[1].as_i64()?),
                3 => Id::Name(e[2].as_str()?.to_owned()),
                _ => return None,
            };
            Some((kind, id))
        })
        .collect()
}

impl PartialEq for Indexed {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Indexed {}

impl PartialOrd for Indexed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Indexed {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Bytes(a), Self::Bytes(b)) => a.cmp(b),
            (Self::Double(a), Self::Double(b)) => a.total_cmp(b),
            (Self::Geo(a1, b1), Self::Geo(a2, b2)) => a1.total_cmp(a2).then(b1.total_cmp(b2)),
            (Self::Key(pr1, ns1, p1), Self::Key(pr2, ns2, p2)) => pr1
                .cmp(pr2)
                .then_with(|| ns1.cmp(ns2))
                .then_with(|| p1.cmp(p2)),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

/// `meaning` 18: an index value, in a projection's results.
const INDEX_VALUE: i32 = 18;

/// The most values of `IN` and `NOT_IN`.
pub(super) const MAX_IN: usize = 30;
pub(super) const MAX_NOT_IN: usize = 10;

/// The index values of `entity` for property `path` (dotted into entity values, not into the
/// entity values of an array, which the official emulator does not index), each once.
pub fn index_values(properties: &Properties, path: &str) -> Vec<(Indexed, Value)> {
    let mut out: Vec<(Indexed, Value)> = Vec::new();
    collect(properties, path, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn collect(properties: &Properties, path: &str, out: &mut Vec<(Indexed, Value)>) {
    if let Some(value) = properties.get(path) {
        push_value(value, out);
    }
    for (name, value) in properties {
        let Some(rest) = path
            .strip_prefix(name.as_str())
            .and_then(|r| r.strip_prefix('.'))
        else {
            continue;
        };
        if let Some(ValueType::EntityValue(entity)) = &value.value_type {
            collect(&entity.properties, rest, out);
        }
    }
}

fn push_value(value: &Value, out: &mut Vec<(Indexed, Value)>) {
    if value.exclude_from_indexes {
        return;
    }
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => {
            for element in &array.values {
                if !element.exclude_from_indexes
                    && let Some(indexed) = Indexed::of(element)
                {
                    out.push((indexed, element.clone()));
                }
            }
        }
        _ => {
            if let Some(indexed) = Indexed::of(value) {
                out.push((indexed, value.clone()));
            }
        }
    }
}

/// A filter, checked.
#[derive(Debug, Clone)]
pub enum Cond {
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Prop {
        name: String,
        op: Op,
        values: Vec<Indexed>,
    },
    Key {
        op: Op,
        keys: Vec<Indexed>,
    },
    Ancestor(Vec<(String, Id)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
}

impl Op {
    fn inequality(self) -> bool {
        matches!(
            self,
            Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge | Self::NotIn
        )
    }

    /// Whether `value` passes against `operands` (none when the operand was an entity value,
    /// which no index value equals or compares with).
    fn test(self, value: &Indexed, operands: &[Indexed]) -> bool {
        match (self, operands.first()) {
            (Self::In, _) => operands.contains(value),
            (Self::NotIn, _) => !operands.contains(value),
            (Self::Ne, None) => true,
            (_, None) => false,
            (Self::Eq, Some(first)) => value == first,
            (Self::Ne, Some(first)) => value != first,
            (Self::Lt, Some(first)) => value < first,
            (Self::Le, Some(first)) => value <= first,
            (Self::Gt, Some(first)) => value > first,
            (Self::Ge, Some(first)) => value >= first,
        }
    }
}

/// A query, checked: what [`run`] needs.
pub struct Plan {
    pub project: String,
    pub namespace: String,
    pub kind: Option<String>,
    pub cond: Option<Cond>,
    /// The row's order: property (`None` for the key) and whether descending.
    pub orders: Vec<(Option<String>, bool)>,
    pub row_properties: Vec<String>,
    pub projection: Option<Vec<String>>,
    pub keys_only: bool,
    pub distinct_on: Vec<String>,
    pub offset: usize,
    pub limit: Option<usize>,
    pub start: Option<Position>,
    pub end: Option<Position>,
    /// Whether the filter can match nothing, as the official emulator sees it ([`no_range`]):
    /// then a limit of 0 has no more results either.
    pub nothing: bool,
    /// Whether the key ends the order only because every query's does: the query neither
    /// orders on it nor compares it.
    implicit_key: bool,
    /// The direction of the query's last order, which the orders it implies take.
    last_descending: bool,
}

impl Plan {
    /// Orders the rows by `properties` too, the ones a sum or an average reads (each value of
    /// an array a row of its own), by name: after the query's orders, before the key unless the
    /// query orders on it or compares it, in the direction of the query's last order, as the
    /// official emulator does (`tools/oracle/datastore_queries.py`).
    pub fn order_by_values_of(&mut self, properties: &[String]) {
        let key = if self.implicit_key {
            self.orders.pop()
        } else {
            None
        };
        let mut by_name: Vec<&String> = properties
            .iter()
            .filter(|p| !self.row_properties.contains(p))
            .collect();
        by_name.sort_unstable();
        by_name.dedup();
        for property in by_name {
            self.row_properties.push(property.clone());
            self.orders
                .push((Some(property.clone()), self.last_descending));
        }
        self.orders.extend(key);
    }
}

/// Where a result is in a query's order: its row values and its key (none for a query distinct
/// on properties, as on the official emulator), just past it in the direction of the query's
/// first order (`before`: that order was descending), and whether the query ordered by the key
/// before a property (a cursor the official emulator cannot read).
#[derive(Debug, Clone)]
pub struct Position {
    pub values: Vec<Indexed>,
    pub key: Option<Located>,
    pub before: bool,
    pub key_in_values: bool,
}

impl Position {
    fn encode(&self) -> Vec<u8> {
        let json = json!({
            "v": self.values.iter().map(Indexed::to_json).collect::<Vec<_>>(),
            "n": self.key.as_ref().map(|k| k.namespace.clone()),
            "k": self.key.as_ref().map(|k| path_json(&k.path)),
            "b": self.before,
            "kv": self.key_in_values,
        });
        let mut out = b"hidane-cursor:".to_vec();
        out.extend_from_slice(json.to_string().as_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let text = bytes.strip_prefix(b"hidane-cursor:")?;
        let json: Json = serde_json::from_slice(text).ok()?;
        Some(Self {
            values: json["v"]
                .as_array()?
                .iter()
                .map(Indexed::from_json)
                .collect::<Option<_>>()?,
            key: if json["k"].is_null() {
                None
            } else {
                Some(Located {
                    namespace: json["n"].as_str()?.to_owned(),
                    path: path_from_json(&json["k"])?,
                })
            },
            before: json["b"].as_bool().unwrap_or(false),
            key_in_values: json["kv"].as_bool().unwrap_or(false),
        })
    }
}

fn invalid(message: impl Into<String>) -> Status {
    Status::invalid_argument(message)
}

/// A key's project (the request's when it names none), namespace and path.
fn key_parts(key: &Key, project: &str) -> (String, String, Vec<(String, Id)>) {
    match operand(
        &Value {
            value_type: Some(ValueType::KeyValue(key.clone())),
            ..Value::default()
        },
        project,
    ) {
        Some(Indexed::Key(p, ns, path)) => (p, ns, path),
        _ => unreachable!("a key value"),
    }
}

/// A filter's value, with the request's project in keys that name none.
fn operand(value: &Value, project: &str) -> Option<Indexed> {
    match Indexed::of(value)? {
        Indexed::Key(p, ns, path) if p.is_empty() => {
            Some(Indexed::Key(project.to_owned(), ns, path))
        }
        other => Some(other),
    }
}

/// Whether `bytes` are a cursor this server gave.
pub(super) fn cursor_parses(bytes: &[u8]) -> bool {
    Position::decode(bytes).is_some()
}

/// The condition of a filter `checks` has accepted.
/// Whether every disjunction of `cond`, in disjunctive normal form, bounds a property (or the
/// key) to an empty range by its inequalities: a lower bound past the upper one, the same value
/// with either excluded, or below null, the least value. Equalities are not counted.
fn no_range(cond: &Cond) -> bool {
    fn conjunctions(cond: &Cond) -> Vec<Vec<&Cond>> {
        match cond {
            Cond::Or(children) => children.iter().flat_map(conjunctions).collect(),
            Cond::And(children) => children.iter().fold(vec![Vec::new()], |product, child| {
                let terms = conjunctions(child);
                product
                    .iter()
                    .flat_map(|left| {
                        terms.iter().map(move |right| {
                            left.iter().chain(right).copied().collect::<Vec<&Cond>>()
                        })
                    })
                    .collect()
            }),
            leaf => vec![vec![leaf]],
        }
    }
    fn empty(conjunction: &[&Cond]) -> bool {
        let mut bounds: Vec<(Option<&str>, Op, &Indexed)> = Vec::new();
        for leaf in conjunction {
            match leaf {
                Cond::Prop { name, op, values }
                    if matches!(op, Op::Lt | Op::Le | Op::Gt | Op::Ge) =>
                {
                    bounds.extend(values.first().map(|v| (Some(name.as_str()), *op, v)));
                }
                Cond::Key { op, keys } if matches!(op, Op::Lt | Op::Le | Op::Gt | Op::Ge) => {
                    bounds.extend(keys.first().map(|v| (None, *op, v)));
                }
                _ => {}
            }
        }
        bounds.iter().any(|(name, op, value)| {
            if *op == Op::Lt && **value == Indexed::Null {
                return true;
            }
            matches!(op, Op::Gt | Op::Ge)
                && bounds.iter().any(|(other, upper, bound)| {
                    other == name
                        && matches!(upper, Op::Lt | Op::Le)
                        && match (*value).cmp(bound) {
                            Ordering::Greater => true,
                            Ordering::Equal => *op == Op::Gt || *upper == Op::Lt,
                            Ordering::Less => false,
                        }
                })
        })
    }
    let all = conjunctions(cond);
    !all.is_empty() && all.iter().all(|c| empty(c))
}

/// A filter as the official emulator runs it: its `HAS_ANCESTOR` filters taken out as the
/// query's ancestor (the checks refuse two), over the rest of the filter without them, where a
/// disjunct that was an ancestor alone is dropped. Where every disjunction holds to the same
/// ancestor, as `RunQuery` requires, that is the filter itself.
fn hoisted(filter: &Filter, project: &str) -> Cond {
    fn without(filter: &Filter, project: &str, ancestor: &mut Option<Cond>) -> Option<Cond> {
        match &filter.filter_type {
            Some(FilterType::CompositeFilter(composite)) => {
                let children = composite
                    .filters
                    .iter()
                    .filter_map(|f| without(f, project, ancestor))
                    .collect();
                Some(if composite.op == composite_filter::Operator::Or as i32 {
                    Cond::Or(children)
                } else {
                    Cond::And(children)
                })
            }
            Some(FilterType::PropertyFilter(property))
                if property.op == property_filter::Operator::HasAncestor as i32 =>
            {
                *ancestor = Some(property_condition(property, project));
                None
            }
            _ => Some(condition(filter, project)),
        }
    }
    let mut ancestor = None;
    let rest = without(filter, project, &mut ancestor);
    match (ancestor, rest) {
        (Some(ancestor), Some(rest)) => Cond::And(vec![ancestor, rest]),
        (Some(cond), None) | (None, Some(cond)) => cond,
        (None, None) => Cond::And(Vec::new()),
    }
}

fn condition(filter: &Filter, project: &str) -> Cond {
    match &filter.filter_type {
        Some(FilterType::CompositeFilter(composite)) => {
            let children = composite
                .filters
                .iter()
                .map(|f| condition(f, project))
                .collect();
            if composite.op == composite_filter::Operator::Or as i32 {
                Cond::Or(children)
            } else {
                Cond::And(children)
            }
        }
        Some(FilterType::PropertyFilter(property)) => property_condition(property, project),
        None => Cond::And(Vec::new()),
    }
}

fn property_condition(filter: &PropertyFilter, project: &str) -> Cond {
    use property_filter::Operator as O;
    let op = match O::try_from(filter.op) {
        Ok(O::NotEqual) => Op::Ne,
        Ok(O::LessThan) => Op::Lt,
        Ok(O::LessThanOrEqual) => Op::Le,
        Ok(O::GreaterThan) => Op::Gt,
        Ok(O::GreaterThanOrEqual) => Op::Ge,
        Ok(O::In) => Op::In,
        Ok(O::NotIn) => Op::NotIn,
        Ok(O::HasAncestor) => {
            return match filter.value.as_ref().and_then(|v| v.value_type.as_ref()) {
                Some(ValueType::KeyValue(key)) => Cond::Ancestor(key_parts(key, project).2),
                _ => Cond::And(Vec::new()),
            };
        }
        _ => Op::Eq,
    };
    let name = filter
        .property
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let operands: Vec<&Value> = match filter.value.as_ref() {
        Some(Value {
            value_type: Some(ValueType::ArrayValue(array)),
            ..
        }) if matches!(op, Op::In | Op::NotIn) => array.values.iter().collect(),
        Some(value) => vec![value],
        None => Vec::new(),
    };
    // An entity operand, which an aggregation's query accepts, equals no index value.
    let values: Vec<Indexed> = operands
        .iter()
        .filter_map(|v| operand(v, project))
        .collect();
    if name == "__key__" {
        return Cond::Key { op, keys: values };
    }
    Cond::Prop { name, op, values }
}

/// The properties a filter has an inequality on, in order, and those it has an equality on.
fn filter_properties(cond: &Cond, inequalities: &mut Vec<String>, equalities: &mut Vec<String>) {
    match cond {
        Cond::And(children) | Cond::Or(children) => {
            for child in children {
                filter_properties(child, inequalities, equalities);
            }
        }
        Cond::Prop { name, op, .. } => {
            let list = if op.inequality() {
                inequalities
            } else {
                equalities
            };
            if !list.contains(name) {
                list.push(name.clone());
            }
        }
        Cond::Key { .. } | Cond::Ancestor(_) => {}
    }
}

/// Plans `query` on `database` in `namespace`, once `checks` has accepted it.
pub fn plan(query: &Query, database: &Database, namespace: &str) -> Result<Plan, Status> {
    let kind = query.kind.first().map(|k| k.name.clone());
    let projection: Vec<String> = query
        .projection
        .iter()
        .map(|p| {
            p.property
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default()
        })
        .collect();
    let cond = query.filter.as_ref().map(|f| hoisted(f, &database.project));
    let nothing = cond.as_ref().is_some_and(no_range);
    let (orders, implicit_key, last_descending) = orders(query, cond.as_ref(), &projection);
    let row_properties: Vec<String> = orders.iter().filter_map(|(f, _)| f.clone()).collect();
    let cursor = |bytes: &[u8]| -> Result<Option<Position>, Status> {
        if bytes.is_empty() {
            return Ok(None);
        }
        Position::decode(bytes)
            .map(Some)
            .ok_or_else(|| invalid("Error parsing protocol message"))
    };
    let start = cursor(&query.start_cursor)?;
    let end = cursor(&query.end_cursor)?;
    let keys_only = !projection.is_empty() && projection.iter().all(|p| p == "__key__");
    let projected: Vec<String> = projection
        .iter()
        .filter(|p| *p != "__key__")
        .cloned()
        .collect();
    Ok(Plan {
        project: database.project.clone(),
        namespace: namespace.to_owned(),
        kind,
        cond,
        orders,
        row_properties,
        projection: (!projection.is_empty() && !keys_only).then_some(projected),
        keys_only,
        distinct_on: query.distinct_on.iter().map(|p| p.name.clone()).collect(),
        offset: usize::try_from(query.offset).unwrap_or(0),
        limit: query.limit.map(|n| usize::try_from(n).unwrap_or(0)),
        start,
        end,
        nothing,
        implicit_key,
        last_descending,
    })
}

/// A query's cursors, as the official emulator checks them once the query passed its other
/// checks: each of the query's namespace and kind, not of a query that ordered by the key before
/// a property, and with no more values than the query orders by (an aggregation's sums and
/// averages ordering by their properties too, `read`).
pub(super) fn cursors_match(query: &Query, namespace: &str, read: &[&str]) -> Result<(), Status> {
    let projection: Vec<String> = query
        .projection
        .iter()
        .map(|p| {
            p.property
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default()
        })
        .collect();
    let cond = query.filter.as_ref().map(|f| condition(f, ""));
    let (orders, _, _) = orders(query, cond.as_ref(), &projection);
    let mut ordered: Vec<&str> = orders.iter().filter_map(|(f, _)| f.as_deref()).collect();
    for property in read {
        if !ordered.contains(property) {
            ordered.push(property);
        }
    }
    let ordered = ordered.len();
    let kind = query.kind.first().map(|k| k.name.as_str());
    for bytes in [&query.start_cursor, &query.end_cursor] {
        let Some(position) = Position::decode(bytes) else {
            continue;
        };
        // A cursor without a key (a distinct query's) names no namespace or kind.
        if let Some(key) = &position.key
            && key.namespace != namespace
        {
            return Err(invalid(format!(
                "The query namespace is '{namespace}' but cursor.postfix_position namespace is '{}'.",
                key.namespace
            )));
        }
        if let (Some(kind), Some((cursor_kind, _))) =
            (kind, position.key.as_ref().and_then(|k| k.path.last()))
            && kind != cursor_kind
        {
            return Err(invalid(format!(
                "The query kind is {kind} but cursor.postfix_position.key kind is {cursor_kind}."
            )));
        }
        if position.key_in_values {
            return Err(invalid("index data should not include primary key"));
        }
        if position.values.len() > ordered {
            return Err(invalid("cursor does not match query"));
        }
    }
    Ok(())
}

/// The orders of a query's rows: its explicit orders, then the distinct and inequality
/// properties (by name; the key last when a filter compares it) and the projected properties
/// (by name) not ordered yet, then the key (`None`). What the query does not order by
/// explicitly follows its last explicit order's direction; a key ordered last only for being
/// last, ascending.
/// The query's orders, then the ones it implies, then the key; whether the key is there only as
/// the last of them, and the direction the implied ones take.
fn orders(
    query: &Query,
    cond: Option<&Cond>,
    projection: &[String],
) -> (Vec<(Option<String>, bool)>, bool, bool) {
    let mut inequalities = Vec::new();
    let mut equalities = Vec::new();
    if let Some(cond) = cond {
        filter_properties(cond, &mut inequalities, &mut equalities);
    }
    inequalities.sort();
    let explicit: Vec<(Option<String>, bool)> = query
        .order
        .iter()
        .map(|order| {
            let name = order
                .property
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            (
                (name != "__key__").then_some(name),
                order.direction == Direction::Descending as i32,
            )
        })
        .collect();
    let mut by_name: Vec<String> = projection
        .iter()
        .filter(|p| *p != "__key__")
        .cloned()
        .collect();
    by_name.sort_unstable();
    let distinct_on: Vec<String> = query.distinct_on.iter().map(|p| p.name.clone()).collect();
    let last = explicit.last().is_some_and(|(_, descending)| *descending);
    let mut orders = explicit;
    let add = |orders: &mut Vec<(Option<String>, bool)>, name: Option<&String>, descending| {
        if !orders.iter().any(|(f, _)| f.as_ref() == name) {
            orders.push((name.cloned(), descending));
        }
    };
    for name in distinct_on.iter().chain(&inequalities) {
        add(&mut orders, Some(name), last);
    }
    // The key, when a filter compares it, is the last inequality.
    if cond.is_some_and(key_inequality) {
        add(&mut orders, None, last);
    }
    for name in &by_name {
        add(&mut orders, Some(name), last);
    }
    let implicit_key = orders.iter().all(|(field, _)| field.is_some());
    add(&mut orders, None, false);
    (orders, implicit_key, last)
}

/// Whether a condition compares the key with an inequality.
fn key_inequality(cond: &Cond) -> bool {
    match cond {
        Cond::And(children) | Cond::Or(children) => children.iter().any(key_inequality),
        Cond::Key { op, .. } => op.inequality(),
        Cond::Prop { .. } | Cond::Ancestor(_) => false,
    }
}

/// One result: an entity, and its values of the row properties.
struct Row<'a> {
    key: &'a Located,
    entity: &'a super::Stored,
    values: Vec<(Indexed, Value)>,
}

fn matches(cond: &Cond, row: &Row<'_>, plan: &Plan) -> bool {
    match cond {
        Cond::And(children) => children.iter().all(|c| matches(c, row, plan)),
        Cond::Or(children) => children.iter().any(|c| matches(c, row, plan)),
        // An equality holds of the entity, whichever of its values the row has: a property
        // filtered by equality and ordered has a row for each of its values, as on the official
        // emulator. An inequality holds of the row's value.
        Cond::Prop { name, op, values } => {
            match plan.row_properties.iter().position(|p| p == name) {
                Some(i) if op.inequality() => op.test(&row.values[i].0, values),
                _ => index_values(&row.entity.properties, name)
                    .iter()
                    .any(|(v, _)| op.test(v, values)),
            }
        }
        Cond::Key { op, keys } => {
            let own = Indexed::Key(
                plan.project.clone(),
                row.key.namespace.clone(),
                row.key.path.clone(),
            );
            op.test(&own, keys)
        }
        Cond::Ancestor(path) => row.key.path.starts_with(path),
    }
}

fn compare_rows(a: &Row<'_>, b: &Row<'_>, plan: &Plan) -> Ordering {
    let mut i = 0;
    for (field, descending) in &plan.orders {
        let ordering = match field {
            Some(_) => {
                let o = a.values[i].0.cmp(&b.values[i].0);
                i += 1;
                o
            }
            None => a.key.cmp(b.key),
        };
        let ordering = if *descending {
            ordering.reverse()
        } else {
            ordering
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn position(row: &Row<'_>, plan: &Plan) -> Position {
    let key_at = plan.orders.iter().position(|(field, _)| field.is_none());
    Position {
        values: row.values.iter().map(|(v, _)| v.clone()).collect(),
        key: plan.distinct_on.is_empty().then(|| row.key.clone()),
        before: plan
            .orders
            .first()
            .is_some_and(|(_, descending)| *descending),
        key_in_values: key_at.is_some_and(|at| at + 1 < plan.orders.len()),
    }
}

fn key_value(plan: &Plan, key: &Located) -> Indexed {
    Indexed::Key(
        plan.project.clone(),
        key.namespace.clone(),
        key.path.clone(),
    )
}

/// Whether every property `plan` orders by runs the same way.
fn one_way(plan: &Plan) -> bool {
    let mut directions = plan
        .orders
        .iter()
        .filter(|(f, _)| f.is_some())
        .map(|(_, d)| *d);
    let first = directions.next();
    directions.all(|d| Some(d) == first)
}

/// Whether a cursor's values end before a property `plan` orders by.
fn ends_before_value(plan: &Plan, position: &Position) -> bool {
    matches!(plan.orders.get(position.values.len()), Some((Some(_), _)))
}

/// A cursor just before its row, with a null value, whose values meet properties and end before
/// another, all ordered one way, is an internal error on the official emulator, whatever the rows.
fn cursor_errors(plan: &Plan) -> Result<(), Status> {
    for position in [&plan.start, &plan.end].into_iter().flatten() {
        let values_meet_properties = plan
            .orders
            .iter()
            .take(position.values.len())
            .all(|(field, _)| field.is_some());
        if position.before
            && position.values.contains(&Indexed::Null)
            && values_meet_properties
            && ends_before_value(plan, position)
            && one_way(plan)
        {
            return Err(Status::unknown(""));
        }
    }
    Ok(())
}

/// Where `row` is from `position` in `plan`'s order, as the official emulator compares them: the
/// position's values then its key against the row's, one by one whatever they stand for (a
/// cursor from another query's), in `plan`'s directions; a row the position's values and key
/// match is past it in the direction of its query's first order, as `plan`'s first order sees
/// it. A position without a key, or one whose key meets the key of an order that goes on past
/// it, is matched by its values alone. A position whose values end before a property is an
/// internal error at a row that matches them, when just before its row and the properties run
/// one way, or just past it and they do not ([`cursor_errors`] tells of one that errs whatever
/// the rows); otherwise its key, if any, compares with that property's value.
fn compare_position(row: &Row<'_>, position: &Position, plan: &Plan) -> Result<Ordering, Status> {
    let mut i = 0;
    for (at, (field, descending)) in plan.orders.iter().enumerate() {
        let own = match field {
            Some(_) => {
                i += 1;
                row.values[i - 1].0.clone()
            }
            None => key_value(plan, row.key),
        };
        let at_key = at == position.values.len();
        if at_key && field.is_some() && position.before == one_way(plan) {
            return Err(Status::unknown(""));
        }
        let theirs = match (position.values.get(at), &position.key) {
            (Some(value), _) => value.clone(),
            (None, Some(key)) if at_key && (field.is_some() || at + 1 == plan.orders.len()) => {
                key_value(plan, key)
            }
            _ => break,
        };
        let ordering = own.cmp(&theirs);
        let ordering = if *descending {
            ordering.reverse()
        } else {
            ordering
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
        if at_key {
            break;
        }
    }
    let first_descending = plan
        .orders
        .first()
        .is_some_and(|(_, descending)| *descending);
    Ok(if position.before == first_descending {
        Ordering::Less
    } else {
        Ordering::Greater
    })
}

/// Every combination of `entity`'s values of the row properties.
fn rows<'a>(key: &'a Located, entity: &'a super::Stored, plan: &Plan) -> Vec<Row<'a>> {
    let mut combinations: Vec<Vec<(Indexed, Value)>> = vec![Vec::new()];
    for property in &plan.row_properties {
        let values = index_values(&entity.properties, property);
        if values.is_empty() {
            return Vec::new();
        }
        combinations = combinations
            .into_iter()
            .flat_map(|prefix| {
                values.iter().map(move |v| {
                    let mut row = prefix.clone();
                    row.push(v.clone());
                    row
                })
            })
            .collect();
    }
    combinations
        .into_iter()
        .map(|values| Row {
            key,
            entity,
            values,
        })
        .collect()
}

/// The rows `plan` selects from `entities`, in order, past its cursors, before its offset and
/// limit. Each row of the kind is compared with the cursors, the ones the filter leaves out too.
fn select<'a>(
    plan: &Plan,
    entities: &'a std::collections::BTreeMap<Located, super::Stored>,
) -> Result<Vec<Row<'a>>, Status> {
    cursor_errors(plan)?;
    let mut results: Vec<Row<'a>> = Vec::new();
    for (key, entity) in entities {
        if key.namespace != plan.namespace {
            continue;
        }
        let kind = &key.path.last().expect("a complete key").0;
        match &plan.kind {
            Some(wanted) if wanted != kind => continue,
            None if super::keys::reserved(kind) => continue,
            _ => {}
        }
        for row in rows(key, entity, plan) {
            for cursor in [&plan.start, &plan.end].into_iter().flatten() {
                compare_position(&row, cursor, plan)?;
            }
            if plan.cond.as_ref().is_none_or(|c| matches(c, &row, plan)) {
                results.push(row);
            }
        }
    }
    results.sort_by(|a, b| compare_rows(a, b, plan));
    if !plan.distinct_on.is_empty() {
        let indexes: Vec<usize> = plan
            .distinct_on
            .iter()
            .filter_map(|d| plan.row_properties.iter().position(|p| p == d))
            .collect();
        let mut seen: Vec<Vec<Indexed>> = Vec::new();
        results.retain(|row| {
            let distinct: Vec<Indexed> = indexes.iter().map(|&i| row.values[i].0.clone()).collect();
            if seen.contains(&distinct) {
                false
            } else {
                seen.push(distinct);
                true
            }
        });
    }
    let past = |row: &Row<'_>, cursor: &Position| {
        compare_position(row, cursor, plan).is_ok_and(|o| o == Ordering::Greater)
    };
    if let Some(start) = &plan.start {
        results.retain(|row| past(row, start));
    }
    if let Some(end) = &plan.end {
        results.retain(|row| !past(row, end));
    }
    Ok(results)
}

/// A row an aggregation reads: its values of the row properties.
pub struct AggregateRow {
    pub values: Vec<Value>,
}

/// The rows of `plan` an aggregation reads: past its offset, up to its limit. A property the
/// first disjunct an entity meets (in the filter's order, an `IN` a disjunct per value) holds
/// equal to a value reads as that value in each of the entity's rows, as on the official
/// emulator: `b = 1` sums 1 for each value of `[1, 2]`.
pub fn aggregate_rows(
    plan: &Plan,
    entities: &std::collections::BTreeMap<Located, super::Stored>,
) -> Result<Vec<AggregateRow>, Status> {
    let results = select(plan, entities)?;
    let skipped = plan.offset.min(results.len());
    let after = &results[skipped..];
    let taken = plan.limit.map_or(after.len(), |l| l.min(after.len()));
    let disjuncts = plan.cond.as_ref().map(disjuncts).unwrap_or_default();
    Ok(after[..taken]
        .iter()
        .map(|row| {
            let mut values: Vec<Value> = row.values.iter().map(|(_, v)| v.clone()).collect();
            let met = disjuncts
                .iter()
                .find(|disjunct| disjunct.iter().all(|leaf| matches(leaf, row, plan)));
            for leaf in met.into_iter().flatten() {
                if let Cond::Prop {
                    name,
                    op: Op::Eq,
                    values: operands,
                } = leaf
                    && let Some(i) = plan.row_properties.iter().position(|p| p == name)
                    && let Some(operand) = operands.first()
                {
                    values[i] = summed(operand);
                }
            }
            AggregateRow { values }
        })
        .collect())
}

/// A condition in disjunctive normal form, in its order: each disjunct's conditions, an `IN` an
/// equality per value.
fn disjuncts(cond: &Cond) -> Vec<Vec<Cond>> {
    match cond {
        Cond::Or(children) => children.iter().flat_map(disjuncts).collect(),
        Cond::And(children) => children.iter().fold(vec![Vec::new()], |product, child| {
            let terms = disjuncts(child);
            product
                .iter()
                .flat_map(|left| {
                    terms
                        .iter()
                        .map(move |right| left.iter().chain(right).cloned().collect::<Vec<Cond>>())
                })
                .collect()
        }),
        Cond::Prop {
            name,
            op: Op::In,
            values,
        } => values
            .iter()
            .map(|value| {
                vec![Cond::Prop {
                    name: name.clone(),
                    op: Op::Eq,
                    values: vec![value.clone()],
                }]
            })
            .collect(),
        leaf => vec![vec![leaf.clone()]],
    }
}

/// An index value as a sum reads it: a number, or nothing it counts.
fn summed(value: &Indexed) -> Value {
    let value_type = match value {
        Indexed::Int(n) => ValueType::IntegerValue(*n),
        Indexed::Double(d) => ValueType::DoubleValue(*d),
        _ => ValueType::NullValue(0),
    };
    Value {
        value_type: Some(value_type),
        ..Value::default()
    }
}

/// Runs `plan` over `entities` (every entity of the database, by key), as of `read_micros`.
pub fn run(
    plan: &Plan,
    entities: &std::collections::BTreeMap<Located, super::Stored>,
    database: &Database,
    read_micros: i64,
) -> Result<QueryResultBatch, Status> {
    let results = select(plan, entities)?;
    let skipped = plan.offset.min(results.len());
    let after_offset = &results[skipped..];
    let taken = plan
        .limit
        .map_or(after_offset.len(), |l| l.min(after_offset.len()));
    let more = plan
        .limit
        .is_some_and(|l| after_offset.len() > l || l == 0 && !after_offset.is_empty());
    let mut batch = QueryResultBatch {
        entity_result_type: if plan.keys_only {
            ResultType::KeyOnly as i32
        } else if plan.projection.is_some() {
            ResultType::Projection as i32
        } else {
            ResultType::Full as i32
        },
        skipped_results: i32::try_from(skipped).unwrap_or(i32::MAX),
        more_results: if more || (plan.limit == Some(0) && !plan.nothing) {
            MoreResultsType::MoreResultsAfterLimit as i32
        } else {
            MoreResultsType::NoMoreResults as i32
        },
        read_time: Some(super::timestamp(read_micros)),
        ..QueryResultBatch::default()
    };
    for row in &after_offset[..taken] {
        let properties = if plan.keys_only {
            Properties::new()
        } else if let Some(projected) = &plan.projection {
            projected
                .iter()
                .filter_map(|name| {
                    let i = plan.row_properties.iter().position(|p| p == name)?;
                    Some((name.clone(), row.values[i].0.projected(database)))
                })
                .collect()
        } else {
            row.entity.properties.clone()
        };
        batch.entity_results.push(EntityResult {
            entity: Some(Entity {
                key: Some(database.key(row.key)),
                properties,
            }),
            cursor: position(row, plan).encode(),
            version: row.entity.version,
            create_time: Some(super::timestamp(row.entity.created)),
            update_time: Some(super::timestamp(row.entity.updated)),
        });
    }
    if let Some(last) = after_offset[..taken].last() {
        batch.end_cursor = position(last, plan).encode();
    }
    batch.snapshot_version = snapshot_version(&batch, read_micros);
    Ok(batch)
}

/// A batch's snapshot version, as the official emulator gives it: its first result's version,
/// or, with none, its read time in seconds.
fn snapshot_version(batch: &QueryResultBatch, read_micros: i64) -> i64 {
    batch
        .entity_results
        .first()
        .map_or(read_micros.div_euclid(1_000_000), |first| first.version)
}

/// `__kind__` and `__namespace__`: the kinds of every namespace, the namespaces (the default
/// one as ID 1), each a key alone, filtered on the key, skipped and limited, in key order
/// (orders by a property change nothing); `None` for another query. Besides the entities' own,
/// they list the kinds and namespaces of the keys read or written (`touched`). They carry no
/// version or cursor, and the batch a snapshot version only when it is empty.
pub fn metadata(
    plan: &Plan,
    entities: &std::collections::BTreeMap<Located, super::Stored>,
    touched: &std::collections::BTreeSet<(String, String)>,
    database: &Database,
    read_micros: i64,
) -> Option<QueryResultBatch> {
    let kind = plan.kind.as_deref()?;
    let known = entities
        .keys()
        .map(|key| {
            (
                key.namespace.as_str(),
                key.path.last().expect("complete").0.as_str(),
            )
        })
        .chain(
            touched
                .iter()
                .map(|(ns, kind)| (ns.as_str(), kind.as_str())),
        );
    let mut names: Vec<Id> = Vec::new();
    match kind {
        "__kind__" => {
            for (_, kind) in known {
                let id = Id::Name(kind.to_owned());
                if !names.contains(&id) {
                    names.push(id);
                }
            }
        }
        "__namespace__" => {
            for (namespace, _) in known {
                let id = if namespace.is_empty() {
                    Id::Id(1)
                } else {
                    Id::Name(namespace.to_owned())
                };
                if !names.contains(&id) {
                    names.push(id);
                }
            }
        }
        _ => return None,
    }
    names.sort();
    let row_entity = super::Stored {
        properties: Properties::new(),
        version: 0,
        created: 0,
        updated: 0,
    };
    // Their keys are in the default namespace, whatever the query's.
    let matching: Vec<Located> = names
        .into_iter()
        .map(|id| Located {
            namespace: String::new(),
            path: vec![(kind.to_owned(), id)],
        })
        .filter(|located| {
            let row = Row {
                key: located,
                entity: &row_entity,
                values: Vec::new(),
            };
            plan.cond
                .as_ref()
                .is_none_or(|cond| matches(cond, &row, plan))
        })
        .collect();
    let skipped = plan.offset.min(matching.len());
    let after_offset = &matching[skipped..];
    let taken = plan
        .limit
        .map_or(after_offset.len(), |l| l.min(after_offset.len()));
    let more = plan.limit.is_some_and(|l| l == 0 || after_offset.len() > l);
    let mut batch = QueryResultBatch {
        entity_result_type: if plan.keys_only {
            ResultType::KeyOnly as i32
        } else {
            ResultType::Full as i32
        },
        skipped_results: i32::try_from(skipped).unwrap_or(i32::MAX),
        more_results: if more {
            MoreResultsType::MoreResultsAfterLimit as i32
        } else {
            MoreResultsType::NoMoreResults as i32
        },
        read_time: Some(super::timestamp(read_micros)),
        ..QueryResultBatch::default()
    };
    for located in &after_offset[..taken] {
        batch.entity_results.push(EntityResult {
            entity: Some(Entity {
                key: Some(database.key(located)),
                properties: Properties::new(),
            }),
            ..EntityResult::default()
        });
    }
    batch.snapshot_version = snapshot_version(&batch, read_micros);
    Some(batch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_ordered_across_types() {
        let ordered = [
            Indexed::Null,
            Indexed::Int(-1),
            Indexed::Int(5),
            Indexed::Bool(false),
            Indexed::Bool(true),
            Indexed::Bytes(b"a".to_vec()),
            Indexed::Bytes(vec![0xc3, 0xa9]),
            Indexed::Double(f64::NEG_INFINITY),
            Indexed::Double(1.0),
            Indexed::Double(f64::INFINITY),
            Indexed::Double(f64::NAN),
            Indexed::Geo(1.0, 2.0),
            Indexed::Key("p".into(), String::new(), vec![("A".into(), Id::Id(5))]),
            Indexed::Key(
                "p".into(),
                String::new(),
                vec![("A".into(), Id::Name("x".into()))],
            ),
            Indexed::Key(
                "p".into(),
                "ns1".into(),
                vec![("A".into(), Id::Name("x".into()))],
            ),
        ];
        for pair in ordered.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?}");
        }
        assert_eq!(Indexed::Double(f64::NAN), Indexed::Double(f64::NAN));
    }

    #[test]
    fn cursors_round_trip() {
        let position = Position {
            values: vec![Indexed::Bytes(b"x".to_vec()), Indexed::Double(1.5)],
            key: Some(Located {
                namespace: "ns".into(),
                path: vec![
                    ("P".into(), Id::Name("p1".into())),
                    ("C".into(), Id::Id(-3)),
                ],
            }),
            before: true,
            key_in_values: true,
        };
        let decoded = Position::decode(&position.encode()).unwrap();
        assert_eq!(decoded.values, position.values);
        assert_eq!(decoded.key, position.key);
        assert!(decoded.before && decoded.key_in_values);
        assert!(Position::decode(&[0, 0, 0]).is_none());
    }
}
