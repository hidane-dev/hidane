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
const MAX_IN: usize = 30;
const MAX_NOT_IN: usize = 10;

/// The index values of `entity` for property `path` (dotted into entity values), each once.
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
        match &value.value_type {
            Some(ValueType::EntityValue(entity)) => collect(&entity.properties, rest, out),
            Some(ValueType::ArrayValue(array)) => {
                for element in &array.values {
                    if let Some(ValueType::EntityValue(entity)) = &element.value_type {
                        collect(&entity.properties, rest, out);
                    }
                }
            }
            _ => {}
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

    fn test(self, value: &Indexed, operands: &[Indexed]) -> bool {
        let first = &operands[0];
        match self {
            Self::Eq => value == first,
            Self::Ne => value != first,
            Self::Lt => value < first,
            Self::Le => value <= first,
            Self::Gt => value > first,
            Self::Ge => value >= first,
            Self::In => operands.contains(value),
            Self::NotIn => !operands.contains(value),
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
}

/// Where a result is in a query's order: its row values and its key.
#[derive(Debug, Clone)]
pub struct Position {
    pub values: Vec<Indexed>,
    pub key: Located,
}

impl Position {
    fn encode(&self) -> Vec<u8> {
        let json = json!({
            "v": self.values.iter().map(Indexed::to_json).collect::<Vec<_>>(),
            "n": self.key.namespace,
            "k": path_json(&self.key.path),
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
            key: Located {
                namespace: json["n"].as_str()?.to_owned(),
                path: path_from_json(&json["k"])?,
            },
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

/// Checks `filter`, in a query on `kind` (`None` without a kind) in `namespace`.
fn check_filter(
    filter: &Filter,
    kind: Option<&str>,
    namespace: &str,
    project: &str,
    in_or: bool,
) -> Result<Cond, Status> {
    match &filter.filter_type {
        None => Err(invalid("a filter must have exactly one of its fields set")),
        Some(FilterType::CompositeFilter(composite)) => {
            if composite.filters.is_empty() {
                return Err(invalid(
                    "A composite filter must have at least one sub-filter.",
                ));
            }
            let or = composite.op == composite_filter::Operator::Or as i32;
            let children = composite
                .filters
                .iter()
                .map(|f| check_filter(f, kind, namespace, project, in_or || or))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(if or {
                Cond::Or(children)
            } else {
                Cond::And(children)
            })
        }
        Some(FilterType::PropertyFilter(property)) => {
            check_property(property, kind, namespace, project, in_or)
        }
    }
}

fn check_property(
    filter: &PropertyFilter,
    kind: Option<&str>,
    namespace: &str,
    project: &str,
    in_or: bool,
) -> Result<Cond, Status> {
    use property_filter::Operator as O;
    let op = match O::try_from(filter.op) {
        Ok(O::Equal) => Op::Eq,
        Ok(O::NotEqual) => Op::Ne,
        Ok(O::LessThan) => Op::Lt,
        Ok(O::LessThanOrEqual) => Op::Le,
        Ok(O::GreaterThan) => Op::Gt,
        Ok(O::GreaterThanOrEqual) => Op::Ge,
        Ok(O::In) => Op::In,
        Ok(O::NotIn) => Op::NotIn,
        Ok(O::HasAncestor) => {
            let name = filter
                .property
                .as_ref()
                .map(|p| p.name.as_str())
                .unwrap_or_default();
            if name != "__key__" {
                return Err(invalid("property must be __key__"));
            }
            let Some(ValueType::KeyValue(key)) =
                filter.value.as_ref().and_then(|v| v.value_type.as_ref())
            else {
                return Err(invalid("HAS_ANCESTOR requires a key value"));
            };
            let (_, ns, path) = key_parts(key, project);
            if ns != namespace {
                return Err(invalid(format!(
                    "The query namespace is '{namespace}' but ancestor namespace is '{ns}'."
                )));
            }
            if in_or {
                return Err(invalid(
                    "All disjunctions must have a common and unique ancestor constraint.",
                ));
            }
            return Ok(Cond::Ancestor(path));
        }
        _ => return Err(invalid("a property filter must specify an operator")),
    };
    let name = filter
        .property
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(invalid("The property.name is the empty string."));
    }
    let Some(value) = filter.value.as_ref().filter(|v| v.value_type.is_some()) else {
        return Err(invalid(format!(
            "The value \"{name}\" does not contain a value."
        )));
    };
    let operands: Vec<&Value> = if matches!(op, Op::In | Op::NotIn) {
        let label = if op == Op::In { "IN" } else { "NOT_IN" };
        let Some(ValueType::ArrayValue(array)) = &value.value_type else {
            return Err(invalid(format!("{label} requires an ArrayValue.")));
        };
        if array.values.is_empty() {
            return Err(invalid(format!(
                "{label} requires an non-empty ArrayValue."
            )));
        }
        if op == Op::In && array.values.len() > MAX_IN {
            return Err(invalid(format!(
                "IN supports up to {MAX_IN} values. There are {} values in the IN filter in your query.",
                array.values.len()
            )));
        }
        if op == Op::NotIn && array.values.len() > MAX_NOT_IN {
            return Err(invalid(format!(
                "NOT_IN supports up to {MAX_NOT_IN} values."
            )));
        }
        array.values.iter().collect()
    } else {
        vec![value]
    };
    if name == "__key__" {
        let keys = operands
            .iter()
            .map(|v| match &v.value_type {
                Some(ValueType::KeyValue(_)) => Ok(operand(v, project).expect("a key")),
                _ => Err(invalid("__key__ filter value must be a Key")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Cond::Key { op, keys });
    }
    if kind.is_none() {
        return Err(invalid(format!("kind is required for filter: {name}")));
    }
    let values = operands
        .iter()
        .map(|v| match &v.value_type {
            Some(ValueType::ArrayValue(_)) => Err(invalid("A list value is not allowed")),
            Some(ValueType::EntityValue(_)) => Err(invalid("An entity value is not allowed")),
            _ => operand(v, project).ok_or_else(|| invalid("An entity value is not allowed")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Cond::Prop { name, op, values })
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

/// Checks `query` on `database` in `namespace`.
pub fn plan(query: &Query, database: &Database, namespace: &str) -> Result<Plan, Status> {
    if query.kind.len() > 1 {
        return Err(invalid("multiple kinds not supported"));
    }
    let kind = match query.kind.first() {
        Some(k) if k.name.is_empty() => return Err(invalid("The kind is the empty string.")),
        Some(k) => Some(k.name.clone()),
        None => None,
    };
    let mut projection: Vec<String> = Vec::new();
    for p in &query.projection {
        let name = p
            .property
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if projection.contains(&name) {
            return Err(invalid("Cannot project a property multiple times."));
        }
        projection.push(name);
    }
    let cond = query
        .filter
        .as_ref()
        .map(|f| check_filter(f, kind.as_deref(), namespace, &database.project, false))
        .transpose()?;
    let mut inequalities = Vec::new();
    let mut equalities = Vec::new();
    if let Some(cond) = &cond {
        filter_properties(cond, &mut inequalities, &mut equalities);
    }
    inequalities.sort();
    let mut explicit: Vec<(Option<String>, bool)> = Vec::new();
    for order in &query.order {
        let name = order
            .property
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let descending = order.direction == Direction::Descending as i32;
        let field = (name != "__key__").then_some(name.clone());
        if explicit.iter().any(|(f, _)| f == &field) {
            return Err(invalid(format!(
                "order by clause cannot contain duplicate fields {name}"
            )));
        }
        explicit.push((field, descending));
    }
    if kind.is_none() && explicit.iter().any(|(f, d)| f.is_some() || *d) {
        return Err(invalid(
            "kind is required for all orders except __key__ ascending",
        ));
    }
    let projected: Vec<String> = projection
        .iter()
        .filter(|p| *p != "__key__")
        .cloned()
        .collect();
    if let Some(p) = projected.iter().find(|p| equalities.contains(p)) {
        let _ = p;
        return Err(invalid(
            "Cannot use projection on a property with an equality filter.",
        ));
    }
    let distinct_on: Vec<String> = query.distinct_on.iter().map(|p| p.name.clone()).collect();
    // The row's order: the explicit orders, then the inequality, distinct and projected
    // properties not ordered yet, ascending, then the key.
    let mut orders = explicit.clone();
    for name in inequalities.iter().chain(&distinct_on).chain(&projected) {
        if !orders.iter().any(|(f, _)| f.as_deref() == Some(name)) {
            orders.push((Some(name.clone()), false));
        }
    }
    if !orders.iter().any(|(f, _)| f.is_none()) {
        orders.push((None, false));
    }
    let row_properties: Vec<String> = orders.iter().filter_map(|(f, _)| f.clone()).collect();
    let limit = match query.limit {
        Some(n) if n < 0 => return Err(invalid("Limit must be non-negative.")),
        Some(n) => Some(usize::try_from(n).unwrap_or(0)),
        None => None,
    };
    if query.offset < 0 {
        return Err(invalid("Offset must be non-negative."));
    }
    let cursor = |bytes: &[u8]| -> Result<Option<Position>, Status> {
        if bytes.is_empty() {
            return Ok(None);
        }
        let position =
            Position::decode(bytes).ok_or_else(|| invalid("Error parsing protocol message"))?;
        if let (Some(kind), Some((cursor_kind, _))) = (&kind, position.key.path.last())
            && kind != cursor_kind
        {
            return Err(invalid(format!(
                "The query kind is {kind} but cursor.postfix_position.key kind is {cursor_kind}."
            )));
        }
        Ok(Some(position))
    };
    let start = cursor(&query.start_cursor)?;
    let end = cursor(&query.end_cursor)?;
    let keys_only = projection == ["__key__"];
    Ok(Plan {
        project: database.project.clone(),
        namespace: namespace.to_owned(),
        kind,
        cond,
        orders,
        row_properties,
        projection: (!projection.is_empty() && !keys_only).then_some(projected),
        keys_only,
        distinct_on,
        offset: usize::try_from(query.offset).unwrap_or(0),
        limit,
        start,
        end,
    })
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
        Cond::Prop { name, op, values } => {
            match plan.row_properties.iter().position(|p| p == name) {
                Some(i) => op.test(&row.values[i].0, values),
                None => index_values(&row.entity.properties, name)
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

fn position(row: &Row<'_>) -> Position {
    Position {
        values: row.values.iter().map(|(v, _)| v.clone()).collect(),
        key: row.key.clone(),
    }
}

fn compare_position(row: &Row<'_>, position: &Position, plan: &Plan) -> Ordering {
    let mut i = 0;
    for (field, descending) in &plan.orders {
        let ordering = match field {
            Some(_) => {
                let o = match position.values.get(i) {
                    Some(v) => row.values[i].0.cmp(v),
                    None => Ordering::Equal,
                };
                i += 1;
                o
            }
            None => row.key.cmp(&position.key),
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
/// limit.
fn select<'a>(
    plan: &Plan,
    entities: &'a std::collections::BTreeMap<Located, super::Stored>,
) -> Vec<Row<'a>> {
    let mut results: Vec<Row<'a>> = Vec::new();
    for (key, entity) in entities {
        if key.namespace != plan.namespace {
            continue;
        }
        let kind = &key.path.last().expect("a complete key").0;
        match &plan.kind {
            Some(wanted) if wanted != kind => continue,
            None if is_reserved(kind) => continue,
            _ => {}
        }
        for row in rows(key, entity, plan) {
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
    if let Some(start) = &plan.start {
        results.retain(|row| compare_position(row, start, plan) == Ordering::Greater);
    }
    if let Some(end) = &plan.end {
        results.retain(|row| compare_position(row, end, plan) != Ordering::Greater);
    }
    results
}

/// A row an aggregation reads: its values of the row properties.
pub struct AggregateRow {
    pub values: Vec<Value>,
}

/// The rows of `plan` an aggregation reads: past its offset, up to its limit.
pub fn aggregate_rows(
    plan: &Plan,
    entities: &std::collections::BTreeMap<Located, super::Stored>,
) -> Vec<AggregateRow> {
    let results = select(plan, entities);
    let skipped = plan.offset.min(results.len());
    let after = &results[skipped..];
    let taken = plan.limit.map_or(after.len(), |l| l.min(after.len()));
    after[..taken]
        .iter()
        .map(|row| AggregateRow {
            values: row.values.iter().map(|(_, v)| v.clone()).collect(),
        })
        .collect()
}

/// Runs `plan` over `entities` (every entity of the database, by key), as of `read_micros`.
pub fn run(
    plan: &Plan,
    entities: &std::collections::BTreeMap<Located, super::Stored>,
    database: &Database,
    read_micros: i64,
) -> QueryResultBatch {
    let results = select(plan, entities);
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
        more_results: if more || plan.limit == Some(0) {
            MoreResultsType::MoreResultsAfterLimit as i32
        } else {
            MoreResultsType::NoMoreResults as i32
        },
        snapshot_version: read_micros,
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
            cursor: position(row).encode(),
            version: row.entity.version,
            create_time: Some(super::timestamp(row.entity.created)),
            update_time: Some(super::timestamp(row.entity.updated)),
        });
    }
    if let Some(last) = after_offset[..taken].last() {
        batch.end_cursor = position(last).encode();
    }
    batch
}

pub fn is_reserved(kind: &str) -> bool {
    kind.len() >= 4 && kind.starts_with("__") && kind.ends_with("__")
}

/// `__kind__` and `__namespace__`: the kinds of every namespace, the namespaces (the default
/// one as ID 1), each a key alone, filtered on the key; `None` for another query.
pub fn metadata(
    plan: &Plan,
    entities: &std::collections::BTreeMap<Located, super::Stored>,
    database: &Database,
    read_micros: i64,
) -> Option<QueryResultBatch> {
    let kind = plan.kind.as_deref()?;
    let mut names: Vec<Id> = Vec::new();
    match kind {
        "__kind__" => {
            for key in entities.keys() {
                let id = Id::Name(key.path.last().expect("complete").0.clone());
                if !names.contains(&id) {
                    names.push(id);
                }
            }
        }
        "__namespace__" => {
            for key in entities.keys() {
                let id = if key.namespace.is_empty() {
                    Id::Id(1)
                } else {
                    Id::Name(key.namespace.clone())
                };
                if !names.contains(&id) {
                    names.push(id);
                }
            }
        }
        _ => return None,
    }
    names.sort();
    let mut batch = QueryResultBatch {
        entity_result_type: if plan.keys_only {
            ResultType::KeyOnly as i32
        } else {
            ResultType::Full as i32
        },
        more_results: MoreResultsType::NoMoreResults as i32,
        read_time: Some(super::timestamp(read_micros)),
        ..QueryResultBatch::default()
    };
    for id in names {
        // Their keys are in the default namespace, whatever the query's.
        let located = Located {
            namespace: String::new(),
            path: vec![(kind.to_owned(), id)],
        };
        let row_entity = super::Stored {
            properties: Properties::new(),
            version: 0,
            created: 0,
            updated: 0,
        };
        let row = Row {
            key: &located,
            entity: &row_entity,
            values: Vec::new(),
        };
        if let Some(cond) = &plan.cond
            && !matches(cond, &row, plan)
        {
            continue;
        }
        batch.entity_results.push(EntityResult {
            entity: Some(Entity {
                key: Some(database.key(&located)),
                properties: Properties::new(),
            }),
            ..EntityResult::default()
        });
    }
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
            key: Located {
                namespace: "ns".into(),
                path: vec![
                    ("P".into(), Id::Name("p1".into())),
                    ("C".into(), Id::Id(-3)),
                ],
            },
        };
        let decoded = Position::decode(&position.encode()).unwrap();
        assert_eq!(decoded.values, position.values);
        assert_eq!(decoded.key, position.key);
        assert!(Position::decode(&[0, 0, 0]).is_none());
    }
}
