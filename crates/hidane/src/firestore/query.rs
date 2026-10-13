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
    hash_order::hash_order,
    normalize::normalize_value,
    order::{compare, is_vector, type_rank},
    path::ResourcePath,
    store::{ReadTime, Store, StoredDocument, identical},
};
use hidane_proto::google::firestore::v1::{
    Cursor, StructuredQuery, Value,
    structured_query::{
        self, Direction, FieldReference, FindNearest, composite_filter, field_filter,
        filter::FilterType, find_nearest::DistanceMeasure, unary_filter,
    },
    value::ValueType,
};
use tonic::Status;

use super::{
    names::{self, Name},
    validate,
};

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
        if path.is_empty() {
            return Err(Status::invalid_argument(
                "Invalid empty property path string.",
            ));
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

/// `find_nearest`: the query's results nearest to a vector, measured on a vector field.
#[derive(Debug, Clone)]
struct Nearest {
    field: FieldPath,
    target: Vec<f64>,
    measure: Measure,
    limit: usize,
    threshold: Option<f64>,
    /// A field name, taken literally (not a path), as on the official emulator.
    result_field: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum Measure {
    Euclidean,
    Cosine,
    DotProduct,
}

impl Measure {
    fn distance(self, a: &[f64], b: &[f64]) -> f64 {
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
        match self {
            Self::Euclidean => a
                .iter()
                .zip(b)
                .map(|(x, y)| (x - y) * (x - y))
                .sum::<f64>()
                .sqrt(),
            // NaN for a zero vector, which then comes last.
            Self::Cosine => 1.0 - dot(a, b) / (dot(a, a) * dot(b, b)).sqrt(),
            Self::DotProduct => dot(a, b),
        }
    }

    /// Nearest first: the smallest distance, or the largest dot product; NaN last.
    fn compare(self, a: f64, b: f64) -> Ordering {
        match (a.is_nan(), b.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => match self {
                Self::DotProduct => b.partial_cmp(&a).unwrap_or(Ordering::Equal),
                Self::Euclidean | Self::Cosine => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
            },
        }
    }

    fn within(self, distance: f64, threshold: f64) -> bool {
        match self {
            Self::DotProduct => distance >= threshold,
            Self::Euclidean | Self::Cosine => distance <= threshold,
        }
    }
}

impl Nearest {
    /// The query vector, read with the rest of what the query names, before an aggregation's
    /// list is checked (#262).
    fn target(find: &FindNearest) -> Result<Vec<f64>, Status> {
        let vector = find.query_vector.clone().unwrap_or_default();
        validate::typed(&vector)?;
        match vector.value_type.as_ref() {
            None => Err(validate::unset()),
            Some(ValueType::MapValue(map)) if is_vector(map) => validate::vector(map),
            Some(_) => Err(Status::invalid_argument(
                "Query Value must be of type vector.",
            )),
        }
    }

    /// Validates the rest of `find` in the official emulator's order, after the rest of
    /// `query`; `target` is its vector ([`Nearest::target`]).
    fn parse(
        find: &FindNearest,
        target: Vec<f64>,
        query: &StructuredQuery,
    ) -> Result<Self, Status> {
        let limit = match find.limit {
            Some(n @ 1..=1000) => usize::try_from(n).unwrap_or(0),
            _ => {
                return Err(Status::invalid_argument(
                    "FindNearest.limit must be a positive integer of no more than 1000",
                ));
            }
        };
        let measure = match DistanceMeasure::try_from(find.distance_measure) {
            Ok(DistanceMeasure::Euclidean) => Measure::Euclidean,
            Ok(DistanceMeasure::Cosine) => Measure::Cosine,
            Ok(DistanceMeasure::DotProduct) => Measure::DotProduct,
            _ => return Err(Status::invalid_argument("Unknown Distance Measure.")),
        };
        let path = find
            .vector_field
            .as_ref()
            .map_or("", |f| f.field_path.as_str());
        if path.is_empty() {
            return Err(Status::invalid_argument(
                "Invalid empty property path string.",
            ));
        }
        // A path, as in production; the official emulator reads `a.b` as one field named
        // `a.b` (docs/parity-exceptions.md).
        let field = FieldPath::parse(path).map_err(Status::invalid_argument)?;
        if query.limit.is_some() {
            return Err(Status::invalid_argument(
                "A query limit cannot be used with FindNearest",
            ));
        }
        if query.offset != 0 {
            return Err(Status::invalid_argument(
                "A query offset cannot be used with FindNearest",
            ));
        }
        if query.start_at.is_some() || query.end_at.is_some() {
            return Err(Status::invalid_argument(
                "A cursor cannot be used with FindNearest",
            ));
        }
        let result_field =
            (!find.distance_result_field.is_empty()).then(|| find.distance_result_field.clone());
        if let Some(name) = &result_field
            && validate::reserved(name)
        {
            return Err(Status::invalid_argument(format!(
                "The distanceResultField.property.name \"{name}\" is reserved."
            )));
        }
        Ok(Self {
            field,
            target,
            measure,
            limit,
            threshold: find.distance_threshold,
            result_field,
        })
    }

    /// The distance of `doc`, if its field holds a vector of the query vector's dimension
    /// within the threshold.
    fn distance(&self, doc: &StoredDocument) -> Option<f64> {
        let fields = doc.fields();
        let Some(ValueType::MapValue(map)) = get(&fields, &self.field)?.value_type.as_ref() else {
            return None;
        };
        if !is_vector(map) {
            return None;
        }
        let vector = validate::vector(map).ok()?;
        if vector.len() != self.target.len() {
            return None;
        }
        let distance = self.measure.distance(&vector, &self.target);
        self.threshold
            .is_none_or(|t| self.measure.within(distance, t))
            .then_some(distance)
    }
}

/// A validated query, with its ordering completed.
#[derive(Debug, Clone)]
pub struct Query {
    parent: ResourcePath,
    source: Source,
    filter: Option<Filter>,
    orders: Vec<Order>,
    /// The orderings the query gives, before `orders` completes them.
    given_orders: Vec<Order>,
    start: Option<Bound>,
    end: Option<Bound>,
    offset: usize,
    limit: Option<usize>,
    projection: Option<Vec<FieldPath>>,
    /// Fields a document must have, besides the ordered ones (those of `sum` and `avg`).
    required: Vec<FieldPath>,
    nearest: Option<Nearest>,
}

/// What a query returns.
pub struct Results {
    pub documents: Vec<Arc<StoredDocument>>,
    /// How many matching documents `offset` skipped.
    pub skipped: usize,
    /// With `find_nearest`, each document's distance.
    pub distances: Vec<f64>,
}

impl Query {
    /// Validates `query` under `parent`, with the official error messages.
    pub fn parse(parent: ResourcePath, query: &StructuredQuery) -> Result<Self, Status> {
        Self::parse_aggregated(parent, query, || Ok(false))
    }

    /// Validates `query` under `parent` in the official emulator's order
    /// (`tools/oracle/queries.py`, `tools/oracle/aggregations.py`, #262): the operators of its
    /// filter ([`check_operators`]); then what each part names and holds (the collection, the
    /// filter's kinds, paths and values, the orders' and the projection's paths, the cursors,
    /// the query vector); then `aggregations`, an aggregation's own checks, which say whether
    /// it has a `sum` or an `avg`; then the filter as a whole (array-contains, equalities on
    /// the key, disjunctions); then the orders (a key before other fields, duplicates), the
    /// limit and the offset, whether the orders need a collection (a `sum` or an `avg` orders
    /// by its field), whether the filter does, the filters on `__name__`, and the rest of
    /// `find_nearest`.
    pub fn parse_aggregated(
        parent: ResourcePath,
        query: &StructuredQuery,
        aggregations: impl FnOnce() -> Result<bool, Status>,
    ) -> Result<Self, Status> {
        check_operators(query)?;
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
        let filter = query
            .r#where
            .as_ref()
            .map(parse_filter)
            .transpose()?
            .flatten();
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

        let bound =
            |cursor: Option<&Cursor>| cursor.map(|c| parse_cursor(c, &explicit)).transpose();
        let start = bound(query.start_at.as_ref())?;
        let end = bound(query.end_at.as_ref())?;
        let target = query
            .find_nearest
            .as_ref()
            .map(Nearest::target)
            .transpose()?;

        let orders_fields = aggregations()?;

        if let Some(filter) = &filter {
            check_filter(filter)?;
        }
        let orders = complete_orders(&explicit, filter.as_ref())?;
        let limit = match query.limit {
            Some(n) if n < 0 => return Err(Status::invalid_argument("limit is negative")),
            Some(n) => Some(usize::try_from(n).unwrap_or(0)),
            None => None,
        };
        if query.offset < 0 {
            return Err(Status::invalid_argument("offset is negative"));
        }
        let offset = usize::try_from(query.offset).unwrap_or(0);
        if kindless && (orders_fields || explicit.iter().any(|o| o.field != Field::Name)) {
            return Err(Status::invalid_argument(
                "kind is required for all orders except __key__ ascending",
            ));
        }
        if let Some(filter) = &filter {
            check_filter_late(filter, kindless)?;
        }
        let nearest = query
            .find_nearest
            .as_ref()
            .zip(target)
            .map(|(find, target)| Nearest::parse(find, target, query))
            .transpose()?;

        Ok(Self {
            parent,
            source,
            filter,
            orders,
            given_orders: explicit,
            start,
            end,
            offset,
            limit,
            projection,
            required: Vec::new(),
            nearest,
        })
    }

    /// Leaves out documents without `fields`, before `offset` and `limit` apply. Aggregating a
    /// field does this on the official emulator, also for the `count` next to it.
    ///
    /// The fields also order the query, as on the official emulator (`tools/oracle/
    /// aggregations.py`, #262): in path order, after the orders the query gives and those its
    /// inequalities imply, before the key, in the direction of the last order it gives; not
    /// at all when the query orders by the key itself. `offset` and `limit` take documents in
    /// that order.
    pub fn require(&mut self, fields: impl IntoIterator<Item = FieldPath>) {
        let mut fields: Vec<FieldPath> = fields.into_iter().collect();
        fields.sort();
        fields.dedup();
        if !self.given_orders.iter().any(|o| o.field == Field::Name) {
            let descending = self.given_orders.last().is_some_and(|o| o.descending);
            let key = self
                .orders
                .iter()
                .position(|o| o.field == Field::Name)
                .unwrap_or(self.orders.len());
            let added: Vec<Order> = fields
                .iter()
                .map(|path| Field::Path(path.clone()))
                .filter(|field| !self.orders.iter().any(|o| o.field == *field))
                .map(|field| Order { field, descending })
                .collect();
            self.orders.splice(key..key, added);
        }
        self.required.extend(fields);
    }

    /// What Security Rules see of the query: the collection it lists (for a collection group,
    /// the parent below which it looks), `request.query` (`offset` is 0 without one, `orderBy`
    /// maps the fields the query orders by, not those completing its order, to `ASC` / `DESC`
    /// in the order of a Java `HashMap`), `request.fields` (the projection, as
    /// `guard::rules_paths` lists it) and the filter that decides it
    /// (`tools/oracle/rules_list.py`, `rules_request_fields.py`).
    pub fn rules_view(&self) -> (ResourcePath, hidane_rules::eval::Query) {
        let (path, group) = match &self.source {
            Source::Collection(id) => (self.parent.child(id.clone()), None),
            Source::Group(id) => (self.parent.clone(), Some(id.clone())),
            Source::Children | Source::Descendants => (self.parent.clone(), Some(String::new())),
        };
        let order_by: Vec<(String, String)> = self
            .given_orders
            .iter()
            .map(|order| {
                // Its segments joined by `.`, nothing escaped.
                let field = match &order.field {
                    Field::Name => "__name__".to_owned(),
                    Field::Path(path) => path.segments().join("."),
                };
                let direction = if order.descending { "DESC" } else { "ASC" };
                (field, direction.to_owned())
            })
            .collect();
        let fields: Vec<&String> = order_by.iter().map(|(field, _)| field).collect();
        let order_by = hash_order(&fields)
            .into_iter()
            .map(|i| order_by[i].clone())
            .collect();
        let query = hidane_rules::eval::Query {
            limit: self.limit.map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
            offset: Some(i64::try_from(self.offset).unwrap_or(i64::MAX)),
            order_by,
            filter: self.filter.as_ref().map(rules_filter),
            group,
            children: matches!(self.source, Source::Children),
            fields: self.projection.as_deref().map(super::guard::rules_paths),
        };
        (path, query)
    }

    /// The collection ID a transaction locks for this query: the official emulator locks
    /// every collection with that ID, whatever its parent.
    pub fn collection_id(&self) -> Option<&str> {
        match &self.source {
            Source::Collection(id) | Source::Group(id) => Some(id),
            Source::Children | Source::Descendants => None,
        }
    }

    /// The field `find_nearest` writes each document's distance into, if any.
    pub fn distance_field(&self) -> Option<&str> {
        self.nearest.as_ref()?.result_field.as_deref()
    }

    /// The fields to return, when the query selects some.
    pub fn projection(&self) -> Option<&[FieldPath]> {
        self.projection.as_deref()
    }

    /// Whether the result can follow changes one document at a time: no limit, offset,
    /// cursor or `find_nearest` makes a document's membership depend on other documents.
    pub fn is_incremental(&self) -> bool {
        self.limit.is_none()
            && self.offset == 0
            && self.start.is_none()
            && self.end.is_none()
            && self.nearest.is_none()
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
        if let Some(nearest) = &self.nearest {
            // No offset, limit or cursor comes with `find_nearest`. Ties keep the query's
            // ordering, as on the official emulator.
            let mut scored: Vec<(Arc<StoredDocument>, f64)> = matches
                .into_iter()
                .filter_map(|(doc, _)| nearest.distance(&doc).map(|d| (doc, d)))
                .collect();
            scored.sort_by(|(_, a), (_, b)| nearest.measure.compare(*a, *b));
            scored.truncate(nearest.limit);
            let (documents, distances) = scored.into_iter().unzip();
            return Results {
                documents,
                skipped: 0,
                distances,
            };
        }
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
        Results {
            documents,
            skipped,
            distances: Vec::new(),
        }
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

/// `None` for a filter that constrains nothing (an empty composite filter). A filter of no kind
/// is refused, wherever it is (`tools/oracle/queries.py`, #262).
fn parse_filter(filter: &structured_query::Filter) -> Result<Option<Filter>, Status> {
    match &filter.filter_type {
        None => Err(Status::invalid_argument("Unknown Filter type.")),
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
        // Its field, then its value (which has a type, #262), then its operator.
        Some(FilterType::FieldFilter(field_filter)) => {
            use field_filter::Operator as F;
            let field = Field::parse(field_filter.field.as_ref())?;
            let value = field_filter.value.clone().unwrap_or_default();
            validate::typed(&value)?;
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
            let value = normalized(value);
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
                // A reference's name is read here; array membership on `__name__` and a value
                // that is not a reference are refused after the orders and the limit
                // (`check_key_values`).
                for value in key_values(op, &value) {
                    if let Some(ValueType::ReferenceValue(name)) = &value.value_type {
                        names::parent(name)?;
                    }
                }
            }
            Ok(Some(Filter::Compare { field, op, value }))
        }
        // Its field, then its operator (#262).
        Some(FilterType::UnaryFilter(unary)) => {
            use unary_filter::Operator as U;
            let Some(unary_filter::OperandType::Field(reference)) = unary.operand_type.as_ref()
            else {
                return Err(Status::invalid_argument(
                    "Unsupported UnaryFilter operand type (non-FIELD).",
                ));
            };
            // A unary filter on `__name__` is refused late, as a key value
            // (`check_key_values`); until then it is an equality or an inequality on the key.
            let field = Field::parse(Some(reference))?;
            let op = match U::try_from(unary.op) {
                Ok(U::IsNan) => UnaryOp::Nan,
                Ok(U::IsNull) => UnaryOp::Null,
                Ok(U::IsNotNan) => UnaryOp::NotNan,
                Ok(U::IsNotNull) => UnaryOp::NotNull,
                Ok(U::Unspecified) | Err(_) => {
                    return Err(Status::invalid_argument("Unknown UnaryFilter operator."));
                }
            };
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

/// The values a `__name__` filter compares with: each of an `in` or `not-in` list.
fn key_values(op: Op, value: &Value) -> &[Value] {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) if matches!(op, Op::In | Op::NotIn) => &array.values,
        _ => std::slice::from_ref(value),
    }
}

/// An equality on `__name__` (`==`, `in`, `is null`, `is NaN`) needs an inequality on
/// `__name__` too when another field has one (`tools/oracle/queries.py`), checked with the
/// filter, before the orders (#262).
fn check_key_equality(leaves: &[(&Field, Leaf)]) -> Result<(), Status> {
    let inequality = |kind: &Leaf| {
        matches!(
            kind,
            Leaf::Compare(
                Op::Less
                    | Op::LessOrEqual
                    | Op::Greater
                    | Op::GreaterOrEqual
                    | Op::NotEqual
                    | Op::NotIn
            ) | Leaf::Unary(UnaryOp::NotNan | UnaryOp::NotNull)
        )
    };
    let key_equality = leaves.iter().any(|(field, kind)| {
        **field == Field::Name
            && matches!(
                kind,
                Leaf::Compare(Op::Equal | Op::In) | Leaf::Unary(UnaryOp::Null | UnaryOp::Nan)
            )
    });
    let key_inequality = leaves
        .iter()
        .any(|(field, kind)| **field == Field::Name && inequality(kind));
    let other_inequality = leaves
        .iter()
        .any(|(field, kind)| **field != Field::Name && inequality(kind));
    if key_equality && other_inequality && !key_inequality {
        return Err(Status::invalid_argument(
            "Equality on key is not allowed if there are other inequality fields and key does \
             not appear in inequalities.",
        ));
    }
    Ok(())
}

/// The filters on `__name__`, last (#262): no array membership, no unary filter, and every
/// value a reference.
fn check_key_values(filter: &Filter, leaves: &[(&Field, Leaf)]) -> Result<(), Status> {
    for (field, kind) in leaves {
        if **field != Field::Name {
            continue;
        }
        match kind {
            Leaf::Compare(Op::ArrayContains | Op::ArrayContainsAny) => {
                return Err(Status::invalid_argument("the name __key__ is reserved"));
            }
            Leaf::Unary(_) => return Err(key_value_required()),
            Leaf::Compare(_) => {}
        }
    }
    let mut keys = Ok(());
    visit_compares(filter, &mut |field, op, value| {
        if keys.is_ok() && *field == Field::Name {
            keys = key_values(op, value).iter().try_for_each(check_key);
        }
    });
    keys
}

fn visit_compares(filter: &Filter, f: &mut dyn FnMut(&Field, Op, &Value)) {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                visit_compares(filter, f);
            }
        }
        Filter::Compare { field, op, value } => f(field, *op, value),
        Filter::Unary { .. } => {}
    }
}

/// Checks that apply to the filter as a whole.
/// The operators of a query's filter, which the official emulator checks before anything else
/// about a query, its transaction included, whatever the filters' fields and values
/// (`tools/oracle/queries.py`, #262): at most one `!=`, `not-in`, `is not NaN` or `is not null`,
/// and `not-in` never next to `in`, `array-contains-any` or an `or`, even one of a single filter.
pub fn check_operators(query: &StructuredQuery) -> Result<(), Status> {
    #[derive(Default)]
    struct Seen {
        negations: usize,
        not_in: bool,
        in_or_any: bool,
        or: bool,
    }
    fn visit(filter: &structured_query::Filter, seen: &mut Seen) {
        match &filter.filter_type {
            Some(FilterType::CompositeFilter(composite)) => {
                if composite.op == composite_filter::Operator::Or as i32 {
                    seen.or = true;
                }
                for filter in &composite.filters {
                    visit(filter, seen);
                }
            }
            Some(FilterType::FieldFilter(field)) => {
                use field_filter::Operator as F;
                match F::try_from(field.op) {
                    Ok(F::NotEqual) => seen.negations += 1,
                    Ok(F::NotIn) => {
                        seen.negations += 1;
                        seen.not_in = true;
                    }
                    Ok(F::In | F::ArrayContainsAny) => seen.in_or_any = true,
                    _ => {}
                }
            }
            Some(FilterType::UnaryFilter(unary)) => {
                use unary_filter::Operator as U;
                if matches!(U::try_from(unary.op), Ok(U::IsNotNan | U::IsNotNull)) {
                    seen.negations += 1;
                }
            }
            None => {}
        }
    }
    let Some(filter) = &query.r#where else {
        return Ok(());
    };
    let mut seen = Seen::default();
    visit(filter, &mut seen);
    if seen.negations > 1 {
        return Err(Status::invalid_argument(
            "Only a single 'NOT_EQUAL', 'NOT_IN', 'IS_NOT_NAN', or 'IS_NOT_NULL' filter allowed per query.",
        ));
    }
    if seen.not_in && (seen.in_or_any || seen.or) {
        return Err(Status::invalid_argument(
            "'NOT_IN' cannot be used in the same query with 'IN', 'ARRAY_CONTAINS_ANY' or 'OR'.",
        ));
    }
    Ok(())
}

/// The filter as a whole, before the orders: at most one array-contains per conjunction, the
/// key's equalities, the disjunctions. Its operators were checked first ([`check_operators`]),
/// the rest comes last ([`check_filter_late`]).
fn check_filter(filter: &Filter) -> Result<(), Status> {
    let mut leaves = Vec::new();
    collect_leaves(filter, &mut leaves);
    let normalized = normalize(filter);
    if array_filters(&normalized) > 1 {
        return Err(Status::failed_precondition(
            "Only a single array-contains clause is allowed in a query",
        ));
    }
    check_key_equality(&leaves)?;
    // Past twice the maximum, the official emulator stops counting.
    let Some(disjunctions) = disjunctions(&normalized).map(|dnf| dnf.len()) else {
        return Err(Status::invalid_argument(format!(
            "Too many disjunctions after normalization, the maximum is {MAX_DISJUNCTIONS}"
        )));
    };
    if disjunctions > MAX_DISJUNCTIONS {
        return Err(Status::invalid_argument(format!(
            "Too many disjunctions after normalization. Result had {disjunctions} disjunctions \
             which is more than the maximum of {MAX_DISJUNCTIONS}"
        )));
    }
    Ok(())
}

/// The last checks of a filter, after the orders, the limit and whether the orders need a
/// collection (`tools/oracle/queries.py`, #262): whether it needs a collection, then the values
/// on `__name__` (`check_key_values`).
fn check_filter_late(filter: &Filter, kindless: bool) -> Result<(), Status> {
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
    check_key_values(filter, &leaves)
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

/// The most `array-contains` and `array-contains-any` filters one conjunction of the filter's
/// disjunctive normal form holds: the branches of an `or` are conjunctions of their own, so two
/// of them there are allowed (`tools/oracle/queries.py`, #262).
fn array_filters(filter: &Filter) -> usize {
    match filter {
        Filter::Or(filters) => filters.iter().map(array_filters).max().unwrap_or(0),
        Filter::And(filters) => filters.iter().map(array_filters).sum(),
        Filter::Compare {
            op: Op::ArrayContains | Op::ArrayContainsAny,
            ..
        } => 1,
        Filter::Compare { .. } | Filter::Unary { .. } => 0,
    }
}

/// The filter as the official emulator normalizes it before counting array-contains filters
/// and disjunctions (`tools/oracle/queries.py`, #262): an `and` in an `and` and an `or` in an
/// `or` flattened into it, a filter that is the same as one before it in its composite left
/// out (an `in`, `not-in` or `array-contains-any` compares its values as a set), and a
/// composite of one filter that filter.
fn normalize(filter: &Filter) -> Filter {
    let (children, and) = match filter {
        Filter::And(children) => (children, true),
        Filter::Or(children) => (children, false),
        leaf => return leaf.clone(),
    };
    let mut flat: Vec<Filter> = Vec::with_capacity(children.len());
    let mut push = |filter: Filter| {
        if !flat.iter().any(|seen| same_filter(seen, &filter)) {
            flat.push(filter);
        }
    };
    for child in children {
        match normalize(child) {
            Filter::And(grandchildren) if and => grandchildren.into_iter().for_each(&mut push),
            Filter::Or(grandchildren) if !and => grandchildren.into_iter().for_each(&mut push),
            other => push(other),
        }
    }
    match flat.len() {
        1 => flat.pop().expect("one filter"),
        _ if and => Filter::And(flat),
        _ => Filter::Or(flat),
    }
}

fn same_filter(a: &Filter, b: &Filter) -> bool {
    match (a, b) {
        (
            Filter::Compare {
                field: fa,
                op: oa,
                value: va,
            },
            Filter::Compare {
                field: fb,
                op: ob,
                value: vb,
            },
        ) => {
            fa == fb
                && oa == ob
                && if matches!(oa, Op::In | Op::NotIn | Op::ArrayContainsAny) {
                    let (a, b) = (array(va), array(vb));
                    a.iter().all(|v| b.iter().any(|w| identical(v, w)))
                        && b.iter().all(|v| a.iter().any(|w| identical(v, w)))
                } else {
                    identical(va, vb)
                }
        }
        (Filter::Unary { field: fa, op: oa }, Filter::Unary { field: fb, op: ob }) => {
            fa == fb && oa == ob
        }
        (Filter::And(a), Filter::And(b)) | (Filter::Or(a), Filter::Or(b)) => {
            a.iter().all(|f| b.iter().any(|g| same_filter(f, g)))
                && b.iter().all(|f| a.iter().any(|g| same_filter(f, g)))
        }
        _ => false,
    }
}

/// One condition of a conjunction in disjunctive normal form: an `in` is an equality per
/// value, an `array-contains-any` an `array-contains` per value.
#[derive(Clone, Copy)]
struct Atom<'a> {
    field: &'a Field,
    kind: Leaf,
    value: Option<&'a Value>,
}

impl Atom<'_> {
    fn same(&self, other: &Self) -> bool {
        self.field == other.field
            && self.kind == other.kind
            && match (self.value, other.value) {
                (Some(a), Some(b)) => identical(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// A conjunction: a set of atoms.
type Conjunction<'a> = Vec<Atom<'a>>;

fn same_conjunction(a: &Conjunction<'_>, b: &Conjunction<'_>) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| x.same(y)))
}

/// The conjunctions a normalized filter becomes in disjunctive normal form, each once (as sets
/// of atoms), or `None` once an `and` would combine more than twice the maximum of them, where
/// the official emulator stops counting (`tools/oracle/queries.py`, #262).
fn disjunctions(filter: &Filter) -> Option<Vec<Conjunction<'_>>> {
    fn add<'a>(out: &mut Vec<Conjunction<'a>>, conjunction: Conjunction<'a>) {
        if !out.iter().any(|seen| same_conjunction(seen, &conjunction)) {
            out.push(conjunction);
        }
    }
    let mut out: Vec<Conjunction<'_>> = Vec::new();
    match filter {
        Filter::Or(children) => {
            for child in children {
                for conjunction in disjunctions(child)? {
                    add(&mut out, conjunction);
                }
            }
        }
        Filter::And(children) => {
            let (first, rest) = children.split_first()?;
            out = disjunctions(first)?;
            for child in rest {
                let terms = disjunctions(child)?;
                if out.len() * terms.len() > 2 * MAX_DISJUNCTIONS {
                    return None;
                }
                let mut product = Vec::new();
                for left in &out {
                    for right in &terms {
                        let mut conjunction: Conjunction<'_> = Vec::new();
                        for atom in left.iter().chain(right) {
                            if !conjunction.iter().any(|seen| seen.same(atom)) {
                                conjunction.push(*atom);
                            }
                        }
                        add(&mut product, conjunction);
                    }
                }
                out = product;
            }
        }
        Filter::Compare {
            field,
            op: op @ (Op::In | Op::ArrayContainsAny),
            value,
        } => {
            let kind = Leaf::Compare(if *op == Op::In {
                Op::Equal
            } else {
                Op::ArrayContains
            });
            for value in array(value) {
                add(
                    &mut out,
                    vec![Atom {
                        field,
                        kind,
                        value: Some(value),
                    }],
                );
            }
        }
        Filter::Compare { field, op, value } => out.push(vec![Atom {
            field,
            kind: Leaf::Compare(*op),
            value: Some(value),
        }]),
        Filter::Unary { field, op } => out.push(vec![Atom {
            field,
            kind: Leaf::Unary(*op),
            value: None,
        }]),
    }
    Some(out)
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

fn more_fields_after_the_key() -> Status {
    Status::invalid_argument("order by clause cannot contain more fields after the key")
}

/// The explicit ordering, then the inequality fields it lacks (in field path order), then
/// `__name__`; added orderings take the direction of the last explicit one.
fn complete_orders(explicit: &[Order], filter: Option<&Filter>) -> Result<Vec<Order>, Status> {
    // The key before other fields comes first, then duplicates (`__name__` twice is the key
    // before another field); whether the orders need a collection is the caller's, after the
    // limit (`tools/oracle/queries.py`, #262).
    if let Some(i) = explicit.iter().position(|o| o.field == Field::Name)
        && i + 1 < explicit.len()
    {
        return Err(more_fields_after_the_key());
    }
    for (i, order) in explicit.iter().enumerate() {
        if explicit[..i].iter().any(|o| o.field == order.field) {
            return Err(Status::invalid_argument(format!(
                "order by clause cannot contain duplicate fields {}",
                order.field.display()
            )));
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
            return Err(more_fields_after_the_key());
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

/// A cursor holds at most one value per explicit ordering, each with a type (#262);
/// `__name__` takes references.
fn parse_cursor(cursor: &Cursor, explicit: &[Order]) -> Result<Bound, Status> {
    if cursor.values.len() > explicit.len() {
        return Err(Status::invalid_argument("Cursor has too many values."));
    }
    let values = cursor
        .values
        .iter()
        .zip(explicit)
        .map(|(value, order)| {
            validate::typed(value)?;
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

/// A filter as Security Rules see it.
fn rules_filter(filter: &Filter) -> hidane_rules::eval::Filter {
    use hidane_rules::eval::{Filter as Rules, FilterOp, Value as RulesValue};
    let rules_field = |field: &Field, op, value| match field {
        Field::Name => Rules::Name { op, value },
        Field::Path(path) => Rules::Field {
            field: path.segments().to_vec(),
            op,
            value,
        },
    };
    match filter {
        Filter::And(filters) => Rules::And(filters.iter().map(rules_filter).collect()),
        Filter::Or(filters) => Rules::Or(filters.iter().map(rules_filter).collect()),
        Filter::Compare { field, op, value } => {
            let op = match op {
                Op::Less => FilterOp::Lt,
                Op::LessOrEqual => FilterOp::Le,
                Op::Greater => FilterOp::Gt,
                Op::GreaterOrEqual => FilterOp::Ge,
                Op::Equal => FilterOp::Eq,
                Op::NotEqual => FilterOp::Ne,
                Op::ArrayContains => FilterOp::ArrayContains,
                Op::In => FilterOp::In,
                Op::ArrayContainsAny => FilterOp::ArrayContainsAny,
                Op::NotIn => FilterOp::NotIn,
            };
            rules_field(field, op, super::guard::value(value))
        }
        Filter::Unary { field, op } => {
            let op = match op {
                UnaryOp::Nan => FilterOp::IsNan,
                UnaryOp::Null => FilterOp::IsNull,
                UnaryOp::NotNan => FilterOp::IsNotNan,
                UnaryOp::NotNull => FilterOp::IsNotNull,
            };
            rules_field(field, op, RulesValue::Null)
        }
    }
}
