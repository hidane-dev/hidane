//! RunQuery: `StructuredQuery` validation and execution, following the official emulator
//! (`tests/fixtures/queries.json`, recorded by `tools/oracle/queries.py`).
//!
//! Queries run without indexes: every candidate document is decoded and tested, then the
//! matches are sorted, so a query costs O(n log n) in the size of its collection. Matching:
//!
//! - A filter never matches a document that lacks the field.
//! - `==`, `in` and `array-contains` compare like the value order, except that null and NaN
//!   match nothing there (the SDKs send `IS_NULL` / `IS_NAN` instead).
//! - Range filters only match values of the filter value's type (numbers mix integers and
//!   doubles), never NaN, and a null or NaN bound matches nothing.
//! - `!=` and `not-in` match every present, non-null value that differs, NaN included; a
//!   `not-in` list containing null matches nothing.
//! - Ordering by a field leaves out documents without it, also for the implicit ordering by
//!   inequality fields.

use std::{cmp::Ordering, sync::Arc};

use hidane_core::{
    field_path::{FieldPath, get},
    normalize::normalize_value,
    order::{compare, type_rank},
    path::ResourcePath,
    store::{ReadTime, Store, StoredDocument},
};
use hidane_proto::google::firestore::v1::{
    Cursor, StructuredQuery, Value,
    structured_query::{
        self, Direction, FieldReference, composite_filter, field_filter, filter::FilterType,
        unary_filter,
    },
    value::ValueType,
};
use tonic::Status;

use super::names::{self, Name};

/// The most comparison values an `in` or `array-contains-any` filter takes.
const MAX_IN: usize = 30;
const MAX_NOT_IN: usize = 10;
/// The most disjunctions a filter may expand to in disjunctive normal form.
const MAX_DISJUNCTIONS: usize = 30;

/// A field of a document, or its name (`__name__`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    Name,
    Path(FieldPath),
}

impl Field {
    fn parse(reference: Option<&FieldReference>) -> Result<Self, Status> {
        let path = reference.map_or("", |r| r.field_path.as_str());
        if path == "__name__" {
            return Ok(Self::Name);
        }
        FieldPath::parse(path)
            .map(Self::Path)
            .map_err(Status::invalid_argument)
    }

    fn display(&self) -> String {
        match self {
            Self::Name => "__key__".to_owned(),
            Self::Path(path) => path.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Equal,
    NotEqual,
    ArrayContains,
    In,
    ArrayContainsAny,
    NotIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnaryOp {
    Nan,
    Null,
    NotNan,
    NotNull,
}

#[derive(Debug, Clone)]
enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Compare { field: Field, op: Op, value: Value },
    Unary { field: Field, op: UnaryOp },
}

#[derive(Debug, Clone)]
struct Order {
    field: Field,
    descending: bool,
}

#[derive(Debug, Clone)]
struct Bound {
    values: Vec<Value>,
    before: bool,
}

/// Where a query looks for documents.
#[derive(Debug, Clone)]
enum Source {
    /// The collection `collection_id` directly under the parent.
    Collection(String),
    /// Every collection named `collection_id` below the parent (a collection group).
    Group(String),
    /// Every collection directly under the parent (`from` with an empty collection ID).
    Children,
    /// Every document below the parent.
    Descendants,
}

/// A validated query, with its ordering completed.
#[derive(Debug, Clone)]
pub struct Query {
    parent: ResourcePath,
    source: Source,
    filter: Option<Filter>,
    orders: Vec<Order>,
    start: Option<Bound>,
    end: Option<Bound>,
    offset: usize,
    limit: Option<usize>,
    projection: Option<Vec<FieldPath>>,
    /// Fields a document must have, besides the ordered ones (those of `sum` and `avg`).
    required: Vec<FieldPath>,
}

/// What a query returns.
pub struct Results {
    pub documents: Vec<Arc<StoredDocument>>,
    /// How many matching documents `offset` skipped.
    pub skipped: usize,
}

impl Query {
    /// Validates `query` under `parent`, with the official error messages.
    pub fn parse(parent: ResourcePath, query: &StructuredQuery) -> Result<Self, Status> {
        if query.find_nearest.is_some() {
            return Err(Status::unimplemented(
                "find_nearest is not implemented yet (https://github.com/hidane-dev/hidane/issues/118)",
            ));
        }
        let source = match query.from.as_slice() {
            [] => Source::Children,
            [selector] if selector.collection_id.is_empty() => {
                if selector.all_descendants {
                    Source::Descendants
                } else {
                    Source::Children
                }
            }
            [selector] => {
                names::query_collection_id(&selector.collection_id)?;
                if selector.all_descendants {
                    Source::Group(selector.collection_id.clone())
                } else {
                    Source::Collection(selector.collection_id.clone())
                }
            }
            _ => {
                return Err(Status::invalid_argument(
                    "StructuredQuery.from cannot have more than one collection selector.",
                ));
            }
        };
        let kindless = matches!(source, Source::Children | Source::Descendants);
        let limit = match query.limit {
            Some(n) if n < 0 => return Err(Status::invalid_argument("limit is negative")),
            Some(n) => Some(usize::try_from(n).unwrap_or(0)),
            None => None,
        };
        if query.offset < 0 {
            return Err(Status::invalid_argument("offset is negative"));
        }
        let offset = usize::try_from(query.offset).unwrap_or(0);

        let filter = query
            .r#where
            .as_ref()
            .map(parse_filter)
            .transpose()?
            .flatten();
        if let Some(filter) = &filter {
            check_filter(filter, kindless)?;
        }

        let explicit = query
            .order_by
            .iter()
            .map(|order| {
                Ok(Order {
                    field: Field::parse(order.field.as_ref())?,
                    descending: order.direction == Direction::Descending as i32,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let orders = complete_orders(&explicit, filter.as_ref(), kindless)?;

        let bound =
            |cursor: Option<&Cursor>| cursor.map(|c| parse_cursor(c, &explicit)).transpose();
        let start = bound(query.start_at.as_ref())?;
        let end = bound(query.end_at.as_ref())?;

        let projection = query
            .select
            .as_ref()
            .map(|select| {
                let mut paths = Vec::new();
                for field in &select.fields {
                    if let Field::Path(path) = Field::parse(Some(field))? {
                        paths.push(path);
                    }
                }
                Ok::<_, Status>(paths)
            })
            .transpose()?;

        Ok(Self {
            parent,
            source,
            filter,
            orders,
            start,
            end,
            offset,
            limit,
            projection,
            required: Vec::new(),
        })
    }

    /// Leaves out documents without `fields`, before `offset` and `limit` apply. Aggregating a
    /// field does this on the official emulator, also for the `count` next to it.
    pub fn require(&mut self, fields: impl IntoIterator<Item = FieldPath>) {
        self.required.extend(fields);
    }

    /// The collection ID a transaction locks for this query: the official emulator locks
    /// every collection with that ID, whatever its parent.
    pub fn collection_id(&self) -> Option<&str> {
        match &self.source {
            Source::Collection(id) | Source::Group(id) => Some(id),
            Source::Children | Source::Descendants => None,
        }
    }

    /// The fields to return, when the query selects some.
    pub fn projection(&self) -> Option<&[FieldPath]> {
        self.projection.as_deref()
    }

    /// Whether the result can follow changes one document at a time: no limit, offset or
    /// cursor makes a document's membership depend on other documents.
    pub fn is_incremental(&self) -> bool {
        self.limit.is_none() && self.offset == 0 && self.start.is_none() && self.end.is_none()
    }

    /// Whether a document at `path` is in the collections this query reads.
    pub fn covers(&self, path: &ResourcePath) -> bool {
        if !path.is_document() || path.len() <= self.parent.len() || !path.starts_with(&self.parent)
        {
            return false;
        }
        match &self.source {
            Source::Collection(id) => {
                path.len() == self.parent.len() + 2 && path.collection_id() == Some(id.as_str())
            }
            Source::Group(id) => path.collection_id() == Some(id.as_str()),
            Source::Children => path.len() == self.parent.len() + 2,
            Source::Descendants => true,
        }
    }

    /// Whether `doc`, which must be in a collection the query reads, matches the filter and has
    /// every ordered and required field. For an incremental query, that is membership.
    pub fn matches(&self, database: &str, doc: &StoredDocument) -> bool {
        self.sort_key(database, doc).is_some()
    }

    /// The values `doc` sorts by, or `None` when the query leaves it out.
    fn sort_key(&self, database: &str, doc: &StoredDocument) -> Option<Vec<Value>> {
        let fields = doc.fields();
        let name = reference(database, &doc.path);
        let field_value = |field: &Field| match field {
            Field::Name => Some(&name),
            Field::Path(path) => get(&fields, path),
        };
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !matches_filter(f, &field_value))
            || self
                .required
                .iter()
                .any(|path| get(&fields, path).is_none())
        {
            return None;
        }
        self.orders
            .iter()
            .map(|order| field_value(&order.field).cloned())
            .collect()
    }

    pub fn run(&self, store: &dyn Store, database: &str, at: ReadTime) -> Results {
        let mut matches: Vec<(Arc<StoredDocument>, Vec<Value>)> = Vec::new();
        let mut visit = |doc: &Arc<StoredDocument>| {
            if let Some(key) = self.sort_key(database, doc) {
                matches.push((Arc::clone(doc), key));
            }
            std::ops::ControlFlow::Continue(())
        };
        match &self.source {
            Source::Collection(id) => {
                store.scan_collection(database, &self.parent.child(id.clone()), at, &mut visit);
            }
            Source::Group(id) => {
                store.scan_collection_group(database, &self.parent, id, at, &mut visit);
            }
            Source::Children => {
                for id in store.list_collection_ids(database, &self.parent, at) {
                    store.scan_collection(database, &self.parent.child(id), at, &mut visit);
                }
            }
            Source::Descendants => {
                store.scan_descendants(database, &self.parent, at, &mut visit);
            }
        }

        matches.sort_by(|(_, a), (_, b)| self.compare_keys(a, b));
        let in_range = |key: &[Value]| {
            let after_start =
                self.start
                    .as_ref()
                    .is_none_or(|start| match self.compare_to_cursor(key, start) {
                        Ordering::Greater => true,
                        Ordering::Equal => start.before,
                        Ordering::Less => false,
                    });
            let before_end =
                self.end
                    .as_ref()
                    .is_none_or(|end| match self.compare_to_cursor(key, end) {
                        Ordering::Less => true,
                        Ordering::Equal => !end.before,
                        Ordering::Greater => false,
                    });
            after_start && before_end
        };
        let in_range: Vec<_> = matches
            .into_iter()
            .filter(|(_, key)| in_range(key))
            .map(|(doc, _)| doc)
            .collect();
        let skipped = self.offset.min(in_range.len());
        let documents = in_range
            .into_iter()
            .skip(self.offset)
            .take(self.limit.unwrap_or(usize::MAX))
            .collect();
        Results { documents, skipped }
    }

    fn compare_keys(&self, a: &[Value], b: &[Value]) -> Ordering {
        for ((order, a), b) in self.orders.iter().zip(a).zip(b) {
            let ordering = compare(a, b);
            if ordering != Ordering::Equal {
                return if order.descending {
                    ordering.reverse()
                } else {
                    ordering
                };
            }
        }
        Ordering::Equal
    }

    /// How `key` compares to the cursor position, on the cursor's leading fields.
    fn compare_to_cursor(&self, key: &[Value], cursor: &Bound) -> Ordering {
        for ((order, a), b) in self.orders.iter().zip(key).zip(&cursor.values) {
            let ordering = compare(a, b);
            if ordering != Ordering::Equal {
                return if order.descending {
                    ordering.reverse()
                } else {
                    ordering
                };
            }
        }
        Ordering::Equal
    }
}

fn reference(database: &str, path: &ResourcePath) -> Value {
    Value {
        value_type: Some(ValueType::ReferenceValue(Name::document_name(
            database, path,
        ))),
    }
}

/// `None` for a filter that constrains nothing (an empty composite filter).
fn parse_filter(filter: &structured_query::Filter) -> Result<Option<Filter>, Status> {
    match &filter.filter_type {
        None => Ok(None),
        Some(FilterType::CompositeFilter(composite)) => {
            let combine: fn(Vec<Filter>) -> Filter =
                match composite_filter::Operator::try_from(composite.op) {
                    Ok(composite_filter::Operator::And) => Filter::And,
                    Ok(composite_filter::Operator::Or) => Filter::Or,
                    _ => {
                        return Err(Status::invalid_argument(
                            "Unsupported CompositeFilter operator.",
                        ));
                    }
                };
            let mut filters = Vec::with_capacity(composite.filters.len());
            for filter in &composite.filters {
                filters.extend(parse_filter(filter)?);
            }
            Ok((!filters.is_empty()).then(|| combine(filters)))
        }
        Some(FilterType::FieldFilter(field_filter)) => {
            use field_filter::Operator as F;
            let op = match F::try_from(field_filter.op) {
                Ok(F::LessThan) => Op::Less,
                Ok(F::LessThanOrEqual) => Op::LessOrEqual,
                Ok(F::GreaterThan) => Op::Greater,
                Ok(F::GreaterThanOrEqual) => Op::GreaterOrEqual,
                Ok(F::Equal) => Op::Equal,
                Ok(F::NotEqual) => Op::NotEqual,
                Ok(F::ArrayContains) => Op::ArrayContains,
                Ok(F::In) => Op::In,
                Ok(F::ArrayContainsAny) => Op::ArrayContainsAny,
                Ok(F::NotIn) => Op::NotIn,
                Ok(F::Unspecified) | Err(_) => {
                    return Err(Status::invalid_argument("Unknown FieldFilter operator."));
                }
            };
            let field = Field::parse(field_filter.field.as_ref())?;
            let value = normalized(field_filter.value.clone().unwrap_or_default());
            if let Some((name, limit)) = match op {
                Op::In => Some(("IN", MAX_IN)),
                Op::ArrayContainsAny => Some(("ARRAY_CONTAINS_ANY", MAX_IN)),
                Op::NotIn => Some(("NOT_IN", MAX_NOT_IN)),
                _ => None,
            } {
                let Some(ValueType::ArrayValue(array)) = &value.value_type else {
                    return Err(Status::invalid_argument(format!(
                        "'{name}' requires an ArrayValue."
                    )));
                };
                if array.values.is_empty() {
                    return Err(Status::invalid_argument(format!(
                        "'{name}' requires an non-empty ArrayValue."
                    )));
                }
                if array.values.len() > limit {
                    return Err(Status::invalid_argument(format!(
                        "'{name}' supports up to {limit} comparison values."
                    )));
                }
            }
            if field == Field::Name {
                if matches!(op, Op::ArrayContains | Op::ArrayContainsAny) {
                    return Err(Status::invalid_argument("the name __key__ is reserved"));
                }
                match &value.value_type {
                    Some(ValueType::ArrayValue(array)) if matches!(op, Op::In | Op::NotIn) => {
                        for value in &array.values {
                            check_key(value)?;
                        }
                    }
                    _ => check_key(&value)?,
                }
            }
            Ok(Some(Filter::Compare { field, op, value }))
        }
        Some(FilterType::UnaryFilter(unary)) => {
            use unary_filter::Operator as U;
            let op = match U::try_from(unary.op) {
                Ok(U::IsNan) => UnaryOp::Nan,
                Ok(U::IsNull) => UnaryOp::Null,
                Ok(U::IsNotNan) => UnaryOp::NotNan,
                Ok(U::IsNotNull) => UnaryOp::NotNull,
                Ok(U::Unspecified) | Err(_) => {
                    return Err(Status::invalid_argument("Unknown UnaryFilter operator."));
                }
            };
            let unary_filter::OperandType::Field(reference) = unary
                .operand_type
                .as_ref()
                .ok_or_else(|| Status::invalid_argument("Unknown UnaryFilter operator."))?;
            let field = Field::parse(Some(reference))?;
            if field == Field::Name {
                return Err(key_value_required());
            }
            Ok(Some(Filter::Unary { field, op }))
        }
    }
}

/// Filter and cursor values compare like stored ones, which are truncated to microseconds.
fn normalized(mut value: Value) -> Value {
    normalize_value(&mut value);
    value
}

fn key_value_required() -> Status {
    Status::invalid_argument("__key__ filter value must be a Key")
}

/// A `__name__` filter value: a reference to a document, in any collection. The official
/// emulator parses it like a parent name, so an odd path gets that message.
fn check_key(value: &Value) -> Result<(), Status> {
    match &value.value_type {
        Some(ValueType::ReferenceValue(name)) => names::parent(name).map(|_| ()),
        _ => Err(key_value_required()),
    }
}

/// Checks that apply to the filter as a whole.
fn check_filter(filter: &Filter, kindless: bool) -> Result<(), Status> {
    let mut leaves = Vec::new();
    collect_leaves(filter, &mut leaves);
    if kindless
        && let Some(field) = leaves
            .iter()
            .map(|leaf| leaf.0)
            .find(|field| **field != Field::Name)
    {
        return Err(Status::invalid_argument(format!(
            "kind is required for filter: {}",
            field.display()
        )));
    }
    let negations = leaves
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                Leaf::Compare(Op::NotEqual | Op::NotIn)
                    | Leaf::Unary(UnaryOp::NotNan | UnaryOp::NotNull)
            )
        })
        .count();
    if negations > 1 {
        return Err(Status::invalid_argument(
            "Only a single 'NOT_EQUAL', 'NOT_IN', 'IS_NOT_NAN', or 'IS_NOT_NULL' filter allowed per query.",
        ));
    }
    let has = |op: Op| leaves.iter().any(|(_, kind)| *kind == Leaf::Compare(op));
    if has(Op::NotIn) && (has(Op::In) || has(Op::ArrayContainsAny) || contains_or(filter)) {
        return Err(Status::invalid_argument(
            "'NOT_IN' cannot be used in the same query with 'IN', 'ARRAY_CONTAINS_ANY' or 'OR'.",
        ));
    }
    let array_filters = leaves
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                Leaf::Compare(Op::ArrayContains | Op::ArrayContainsAny)
            )
        })
        .count();
    if array_filters > 1 {
        return Err(Status::failed_precondition(
            "Only a single array-contains clause is allowed in a query",
        ));
    }
    let disjunctions = disjunctions(filter);
    if disjunctions > MAX_DISJUNCTIONS {
        return Err(Status::invalid_argument(format!(
            "Too many disjunctions after normalization. Result had {disjunctions} disjunctions \
             which is more than the maximum of {MAX_DISJUNCTIONS}"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
    Compare(Op),
    Unary(UnaryOp),
}

fn collect_leaves<'a>(filter: &'a Filter, out: &mut Vec<(&'a Field, Leaf)>) {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                collect_leaves(filter, out);
            }
        }
        Filter::Compare { field, op, .. } => out.push((field, Leaf::Compare(*op))),
        Filter::Unary { field, op } => out.push((field, Leaf::Unary(*op))),
    }
}

fn contains_or(filter: &Filter) -> bool {
    match filter {
        Filter::Or(filters) => filters.len() > 1 || filters.iter().any(contains_or),
        Filter::And(filters) => filters.iter().any(contains_or),
        Filter::Compare { .. } | Filter::Unary { .. } => false,
    }
}

/// How many conjunctions the filter becomes in disjunctive normal form, where `in` and
/// `array-contains-any` count one per value.
fn disjunctions(filter: &Filter) -> usize {
    match filter {
        Filter::Or(filters) => filters.iter().map(disjunctions).sum(),
        Filter::And(filters) => filters
            .iter()
            .map(disjunctions)
            .fold(1, usize::saturating_mul),
        Filter::Compare {
            op: Op::In | Op::ArrayContainsAny,
            value,
            ..
        } => match &value.value_type {
            Some(ValueType::ArrayValue(array)) => array.values.len(),
            _ => 1,
        },
        Filter::Compare { .. } | Filter::Unary { .. } => 1,
    }
}

/// Fields that need an implicit ordering: those of range, `!=`, `not-in`, `is not null` and
/// `is not NaN` filters, anywhere in the filter.
fn inequality_fields(filter: &Filter, out: &mut Vec<FieldPath>) {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                inequality_fields(filter, out);
            }
        }
        Filter::Compare {
            field: Field::Path(path),
            op:
                Op::Less | Op::LessOrEqual | Op::Greater | Op::GreaterOrEqual | Op::NotEqual | Op::NotIn,
            ..
        }
        | Filter::Unary {
            field: Field::Path(path),
            op: UnaryOp::NotNan | UnaryOp::NotNull,
        } => out.push(path.clone()),
        Filter::Compare { .. } | Filter::Unary { .. } => {}
    }
}

/// The explicit ordering, then the inequality fields it lacks (in field path order), then
/// `__name__`; added orderings take the direction of the last explicit one.
fn complete_orders(
    explicit: &[Order],
    filter: Option<&Filter>,
    kindless: bool,
) -> Result<Vec<Order>, Status> {
    for (i, order) in explicit.iter().enumerate() {
        if explicit[..i].iter().any(|o| o.field == order.field) {
            return Err(Status::invalid_argument(format!(
                "order by clause cannot contain duplicate fields {}",
                order.field.display()
            )));
        }
        if kindless && order.field != Field::Name {
            return Err(Status::invalid_argument(
                "kind is required for all orders except __key__ ascending",
            ));
        }
    }
    let descending = explicit.last().is_some_and(|o| o.descending);
    let mut orders = explicit.to_vec();
    let mut implicit = Vec::new();
    if let Some(filter) = filter {
        inequality_fields(filter, &mut implicit);
    }
    implicit.sort();
    implicit.dedup();
    for path in implicit {
        let field = Field::Path(path);
        if !orders.iter().any(|o| o.field == field) {
            orders.push(Order { field, descending });
        }
    }
    match orders.iter().position(|o| o.field == Field::Name) {
        Some(i) if i + 1 < orders.len() => {
            return Err(Status::invalid_argument(
                "order by clause cannot contain more fields after the key",
            ));
        }
        Some(_) => {}
        None => orders.push(Order {
            field: Field::Name,
            descending,
        }),
    }
    // The official emulator scans the key range for these, and cannot do so backwards.
    if let [only] = orders.as_slice()
        && only.descending
        && !filter.is_some_and(filters_a_property)
    {
        return Err(Status::failed_precondition(
            "Firestore does not support descending key scans",
        ));
    }
    Ok(orders)
}

fn filters_a_property(filter: &Filter) -> bool {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => filters.iter().any(filters_a_property),
        Filter::Compare { field, .. } | Filter::Unary { field, .. } => *field != Field::Name,
    }
}

/// A cursor holds at most one value per explicit ordering; `__name__` takes references.
fn parse_cursor(cursor: &Cursor, explicit: &[Order]) -> Result<Bound, Status> {
    if cursor.values.len() > explicit.len() {
        return Err(Status::invalid_argument("Cursor has too many values."));
    }
    let values = cursor
        .values
        .iter()
        .zip(explicit)
        .map(|(value, order)| {
            if order.field == Field::Name {
                if !matches!(value.value_type, Some(ValueType::ReferenceValue(_))) {
                    return Err(Status::invalid_argument(
                        "Cursor __key__ value is not a document reference.",
                    ));
                }
                check_key(value)?;
            }
            Ok(normalized(value.clone()))
        })
        .collect::<Result<_, Status>>()?;
    Ok(Bound {
        values,
        before: cursor.before,
    })
}

fn is_null(value: &Value) -> bool {
    matches!(value.value_type, None | Some(ValueType::NullValue(_)))
}

fn is_nan(value: &Value) -> bool {
    matches!(value.value_type, Some(ValueType::DoubleValue(d)) if d.is_nan())
}

/// Equality for `==`, `in`, `array-contains` and their negations: null and NaN equal nothing.
fn equal(a: &Value, b: &Value) -> bool {
    !is_null(a) && !is_nan(a) && !is_nan(b) && compare(a, b) == Ordering::Equal
}

fn array(value: &Value) -> &[Value] {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => &array.values,
        _ => &[],
    }
}

fn matches_filter<'a>(filter: &Filter, field_value: &impl Fn(&Field) -> Option<&'a Value>) -> bool {
    match filter {
        Filter::And(filters) => filters.iter().all(|f| matches_filter(f, field_value)),
        Filter::Or(filters) => filters.iter().any(|f| matches_filter(f, field_value)),
        Filter::Unary { field, op } => field_value(field).is_some_and(|v| match op {
            UnaryOp::Null => is_null(v),
            UnaryOp::NotNull => !is_null(v),
            UnaryOp::Nan => is_nan(v),
            UnaryOp::NotNan => !is_null(v) && !is_nan(v),
        }),
        Filter::Compare { field, op, value } => {
            field_value(field).is_some_and(|v| matches_compare(v, *op, value))
        }
    }
}

fn matches_compare(v: &Value, op: Op, operand: &Value) -> bool {
    let range = |accept: fn(Ordering) -> bool| {
        !is_null(operand)
            && !is_nan(operand)
            && !is_nan(v)
            && type_rank(v) == type_rank(operand)
            && accept(compare(v, operand))
    };
    match op {
        Op::Less => range(Ordering::is_lt),
        Op::LessOrEqual => range(Ordering::is_le),
        Op::Greater => range(Ordering::is_gt),
        Op::GreaterOrEqual => range(Ordering::is_ge),
        Op::Equal => equal(v, operand),
        Op::NotEqual => !is_null(operand) && !is_null(v) && !equal(v, operand),
        Op::ArrayContains => array(v).iter().any(|e| equal(e, operand)),
        Op::In => array(operand).iter().any(|o| equal(v, o)),
        Op::ArrayContainsAny => array(v)
            .iter()
            .any(|e| array(operand).iter().any(|o| equal(e, o))),
        Op::NotIn => {
            !is_null(v)
                && !array(operand).iter().any(is_null)
                && !array(operand).iter().any(|o| equal(v, o))
        }
    }
}
