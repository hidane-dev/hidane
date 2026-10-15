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
    property_path::{self, PropertyPath},
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
    /// A path with array offsets (`a[0]`), which the official emulator reads, compares and
    /// prints like another until it refuses it, late ([`Field::check`]); it reaches nothing.
    Indexed(PropertyPath),
}

impl Field {
    /// Reads a filter's or an order's path ([`PropertyPath::read`]); what it may hold is
    /// checked late ([`Field::check`]).
    fn parse(reference: Option<&FieldReference>) -> Result<Self, Status> {
        let path = PropertyPath::read(reference.map_or("", |r| r.field_path.as_str()))?;
        Ok(if path.is_name() {
            Self::Name
        } else if path.has_offsets() {
            Self::Indexed(path)
        } else {
            Self::Path(path.field_path())
        })
    }

    /// The path as the official emulator's messages print it: segments unquoted.
    fn display(&self) -> String {
        match self {
            Self::Name => "__key__".to_owned(),
            Self::Path(path) => property_path::shown(path),
            Self::Indexed(path) => path.to_string(),
        }
    }

    /// What a filter's or an order's path may hold ([`PropertyPath::check_query`]), checked
    /// after the orders, the limit and whether the orders need a collection (#262).
    fn check(&self) -> Result<(), Status> {
        match self {
            Self::Name => Ok(()),
            Self::Path(path) => PropertyPath::of(path).check_query(),
            Self::Indexed(path) => path.check_query(),
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
    /// `find_nearest` as the official emulator reads it with the rest of what the query
    /// names, before an aggregation's list is checked (#262): its vector, its limit, its
    /// measure, then its field. Its interplay with the query comes last ([`Nearest::check`]).
    fn read(find: &FindNearest) -> Result<Self, Status> {
        let vector = find.query_vector.clone().unwrap_or_default();
        validate::query_value(&vector)?;
        let target = match vector.value_type.as_ref() {
            None => return Err(validate::unset()),
            Some(ValueType::MapValue(map)) if is_vector(map) => validate::vector(map)?,
            Some(_) => {
                return Err(Status::invalid_argument(
                    "Query Value must be of type vector.",
                ));
            }
        };
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
        // A path, as in production; the official emulator reads `a.b` as one field named
        // `a.b` (docs/parity-exceptions.md). It is read as other paths are, and nothing more
        // is checked (#262).
        let field = PropertyPath::read(path)?.field_path();
        Ok(Self {
            field,
            target,
            measure,
            limit,
            threshold: find.distance_threshold,
            result_field: (!find.distance_result_field.is_empty())
                .then(|| find.distance_result_field.clone()),
        })
    }

    /// The rest of `find_nearest`, after the rest of `query` but whether the orders need a
    /// collection and the late checks of the filter: no limit, offset or cursor next to it,
    /// and a distance field that is not reserved.
    fn check(&self, query: &StructuredQuery) -> Result<(), Status> {
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
        if let Some(name) = &self.result_field
            && validate::reserved(name)
        {
            return Err(Status::invalid_argument(format!(
                "The distanceResultField.property.name \"{name}\" is reserved."
            )));
        }
        Ok(())
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

/// A query as the official emulator reads it, before Security Rules and the read time
/// (`tools/oracle/rules_list.py`, `tools/oracle/queries.py`, #262), with the rest still to check
/// ([`Unchecked::check`]).
pub struct Unchecked<'a> {
    database: &'a str,
    parent: ResourcePath,
    query: &'a StructuredQuery,
    source: Source,
    filter: Option<Filter>,
    projection: Option<Vec<FieldPath>>,
    explicit: Vec<Order>,
    start: Option<Bound>,
    end: Option<Bound>,
    nearest: Option<Nearest>,
    /// The fields a `sum` or an `avg` reads, which order the query.
    aggregated: Vec<FieldPath>,
}

/// What Security Rules see of a query: the collection it lists (for a collection group, the
/// parent below which it looks), `request.query` (`offset` is 0 without one, `orderBy` maps
/// the fields the query orders by, not those completing its order, to `ASC` / `DESC` in the
/// order of a Java `HashMap`), `request.fields` (the projection, as `guard::rules_paths` lists
/// it) and the filter that decides it (`tools/oracle/rules_list.py`,
/// `rules_request_fields.py`).
fn rules_view(
    parent: &ResourcePath,
    source: &Source,
    given_orders: &[Order],
    limit: Option<i64>,
    offset: i64,
    filter: Option<&Filter>,
    projection: Option<&[FieldPath]>,
) -> (ResourcePath, hidane_rules::eval::Query) {
    let (path, group) = match source {
        Source::Collection(id) => (parent.child(id.clone()), None),
        Source::Group(id) => (parent.clone(), Some(id.clone())),
        Source::Children | Source::Descendants => (parent.clone(), Some(String::new())),
    };
    let order_by: Vec<(String, String)> = given_orders
        .iter()
        .map(|order| {
            // Its segments joined by `.`, nothing escaped.
            let field = match &order.field {
                Field::Name => "__name__".to_owned(),
                Field::Path(path) => path.segments().join("."),
                Field::Indexed(path) => path.to_string(),
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
        limit,
        offset: Some(offset),
        order_by,
        filter: filter.map(rules_filter),
        group,
        children: matches!(source, Source::Children),
        fields: projection.map(super::guard::rules_paths),
    };
    (path, query)
}

impl Unchecked<'_> {
    /// What Security Rules see of the query, as read: its limit and offset as given, negative
    /// ones too.
    pub fn rules_view(&self) -> (ResourcePath, hidane_rules::eval::Query) {
        rules_view(
            &self.parent,
            &self.source,
            &self.explicit,
            self.query.limit.map(i64::from),
            i64::from(self.query.offset),
            self.filter.as_ref(),
            self.projection.as_deref(),
        )
    }

    /// What Security Rules check first: the filter normalizes to at most 30 disjunctions,
    /// before anything else of the query is checked (`tools/oracle/rules_list.py`, #262).
    pub fn check_disjunctions(&self) -> Result<(), Status> {
        self.filter.as_ref().map_or(Ok(()), check_disjunctions)
    }

    /// Checks the rest of the query, after Security Rules and the read time, in the official
    /// emulator's order (#262): the filter as a whole (array-contains, equalities on the key),
    /// whether the fields a `sum` or an `avg` reads can be ordered
    /// ([`check_index`]), the disjunctions; then the orders (a key before other fields,
    /// duplicates), the limit and the offset, the rest of `find_nearest`, the number of
    /// filters and orders ([`check_terms`]), whether the orders need a collection (a `sum` or
    /// an `avg` orders by its field), whether the filter does, and the filters on `__name__`.
    pub fn check(self) -> Result<Query, Status> {
        let Self {
            database,
            parent,
            query,
            source,
            filter,
            projection,
            explicit,
            start,
            end,
            nearest,
            aggregated,
        } = self;
        let kindless = matches!(source, Source::Children | Source::Descendants);
        // A collection group below a document runs, as in production (D088).
        if let Some(filter) = &filter {
            check_filter(filter)?;
        }
        check_index(&explicit, filter.as_ref(), &aggregated)?;
        if let Some(filter) = &filter {
            check_disjunctions(filter)?;
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
        if let Some(nearest) = &nearest {
            nearest.check(query)?;
        }
        check_terms(
            filter.as_ref(),
            &orders,
            &explicit,
            &aggregated,
            !parent.is_empty(),
        )?;
        if kindless
            && (!aggregated.is_empty()
                || explicit
                    .iter()
                    .any(|o| o.field != Field::Name || o.descending))
        {
            return Err(Status::invalid_argument(
                "kind is required for all orders except __key__ ascending",
            ));
        }
        if let Some(filter) = &filter {
            check_filter_late(filter, kindless, database)?;
        }
        explicit.iter().try_for_each(|order| order.field.check())?;

        Ok(Query {
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
}

impl Query {
    /// Validates `query` under `parent` in `database`, with the official error messages.
    pub fn parse(
        database: &str,
        parent: ResourcePath,
        query: &StructuredQuery,
    ) -> Result<Self, Status> {
        Self::read(database, parent, query, |_| Ok(Vec::new()))?.check()
    }

    /// Reads `query` under `parent` in the official emulator's order (`tools/oracle/
    /// queries.py`, `tools/oracle/aggregations.py`, #262): the operators of its filter
    /// ([`check_operators`]); then what each part names and holds (the collection, the
    /// filter's kinds, paths and values, the orders' and the projection's paths, the cursors,
    /// `find_nearest` but its interplay with the query); then `aggregations`, the list's checks
    /// (with the projection), which say which fields a `sum` or an `avg` reads. The rest is
    /// checked after Security Rules and the read time ([`Unchecked::check`]).
    pub fn read<'a>(
        database: &'a str,
        parent: ResourcePath,
        query: &'a StructuredQuery,
        aggregations: impl FnOnce(Option<&[FieldPath]>) -> Result<Vec<FieldPath>, Status>,
    ) -> Result<Unchecked<'a>, Status> {
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
        let filter = query
            .r#where
            .as_ref()
            .map(parse_filter)
            .transpose()?
            .flatten();
        // The projection, read and checked as a mask is, before the orders (#262).
        let projection = query
            .select
            .as_ref()
            .map(|select| {
                let read = select
                    .fields
                    .iter()
                    .map(|field| PropertyPath::read(&field.field_path))
                    .collect::<Result<Vec<_>, _>>()?;
                read.iter().try_for_each(PropertyPath::check_mask)?;
                Ok::<_, Status>(
                    read.iter()
                        .filter(|path| !path.is_name())
                        .map(PropertyPath::field_path)
                        .collect(),
                )
            })
            .transpose()?;
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

        let bound =
            |cursor: Option<&Cursor>| cursor.map(|c| parse_cursor(c, &explicit)).transpose();
        let start = bound(query.start_at.as_ref())?;
        let end = bound(query.end_at.as_ref())?;
        let nearest = query.find_nearest.as_ref().map(Nearest::read).transpose()?;
        let aggregated = aggregations(projection.as_deref())?;
        Ok(Unchecked {
            database,
            parent,
            query,
            source,
            filter,
            projection,
            explicit,
            start,
            end,
            nearest,
            aggregated,
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

    /// What Security Rules see of the query ([`rules_view`]).
    pub fn rules_view(&self) -> (ResourcePath, hidane_rules::eval::Query) {
        rules_view(
            &self.parent,
            &self.source,
            &self.given_orders,
            self.limit.map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
            i64::try_from(self.offset).unwrap_or(i64::MAX),
            self.filter.as_ref(),
            self.projection.as_deref(),
        )
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
            Field::Indexed(_) => None,
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
        // How many documents in range the result needs, when it takes the first ones.
        let wanted = self
            .limit
            .filter(|_| self.nearest.is_none())
            .map(|limit| self.offset.saturating_add(limit));
        // Ordered by name alone, ascending, these scans visit documents in the query's order
        // (the store's keys sort as `__name__` does), so a limited query stops once it has its
        // documents rather than reading the whole collection: a listener runs such a query
        // again on every commit (#295). The children's scans are in order within each
        // collection only.
        let stop_at = wanted.filter(|_| {
            matches!(self.orders.as_slice(), [order] if order.field == Field::Name && !order.descending)
                && !matches!(self.source, Source::Children)
        });
        type Match = (Arc<StoredDocument>, Vec<Value>);
        let mut found = 0;
        let mut matches: Vec<Match> = Vec::new();
        let mut visit = |doc: &Arc<StoredDocument>| {
            if let Some(key) = self.sort_key(database, doc) {
                if let Some(stop_at) = stop_at {
                    found += usize::from(in_range(&key));
                    matches.push((Arc::clone(doc), key));
                    if found >= stop_at {
                        return std::ops::ControlFlow::Break(());
                    }
                } else {
                    matches.push((Arc::clone(doc), key));
                }
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

        let compare = |(_, a): &Match, (_, b): &Match| self.compare_keys(a, b);
        if let Some(nearest) = &self.nearest {
            matches.sort_by(compare);
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
        matches.retain(|(_, key)| in_range(key));
        let skipped = self.offset.min(matches.len());
        // Only the first `wanted` are kept: select them, then sort those. The orders end with
        // `__name__`, so no two keys are equal and this keeps the documents a full sort would.
        if let Some(wanted) = wanted
            && wanted < matches.len()
        {
            if wanted > 0 {
                matches.select_nth_unstable_by(wanted - 1, compare);
            }
            matches.truncate(wanted);
        }
        matches.sort_by(compare);
        let documents = matches
            .into_iter()
            .skip(self.offset)
            .take(self.limit.unwrap_or(usize::MAX))
            .map(|(doc, _)| doc)
            .collect();
        Results {
            documents,
            skipped,
            distances: Vec::new(),
        }
    }

    /// Whether the query orders by the name alone, ascending.
    pub fn orders_by_name(&self) -> bool {
        matches!(
            self.orders.as_slice(),
            [Order {
                field: Field::Name,
                descending: false
            }]
        )
    }

    /// The query's documents after `after` in its order (a page token's document), or all of
    /// them; a document that the query leaves out continues nothing.
    pub fn run_after(
        &self,
        store: &dyn Store,
        database: &str,
        at: ReadTime,
        after: Option<&StoredDocument>,
    ) -> Vec<Arc<StoredDocument>> {
        let documents = self.run(store, database, at).documents;
        let Some(after) = after else {
            return documents;
        };
        let Some(after) = self.sort_key(database, after) else {
            return Vec::new();
        };
        documents
            .into_iter()
            .filter(|doc| {
                self.sort_key(database, doc)
                    .is_some_and(|key| self.compare_keys(&key, &after) == Ordering::Greater)
            })
            .collect()
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
            validate::query_value(&value)?;
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
            // On `__name__`, array membership, a value that is not a reference and a reference
            // that is not a key of the database are refused after the orders and the limit
            // (`check_key_filter`).
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
            // (`check_key_filter`); until then it is an equality or an inequality on the key.
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

/// A `__name__` filter value: a reference to a document of `database`, in any collection, read
/// as a key (`tools/oracle/queries.py`, #262): not empty, of the query's database, its IDs
/// within the key's limits.
fn check_key(value: &Value, database: &str) -> Result<(), Status> {
    let Some(ValueType::ReferenceValue(name)) = &value.value_type else {
        return Err(key_value_required());
    };
    let key = names::reference(name)?;
    if key.path.is_empty() {
        return Err(validate::key_path_empty());
    }
    if key.database != database {
        return Err(Status::invalid_argument(format!(
            "The request was for database '{database}' but was attempting to access database \
             '{}'",
            key.database
        )));
    }
    validate::key(&key.path)
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
/// filter, before the orders (#262). An inequality that matches nothing ([`matches_nothing`])
/// does not count.
fn check_key_equality(filter: &Filter, leaves: &[(&Field, Leaf)]) -> Result<(), Status> {
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
    let mut others = Vec::new();
    inequality_fields(filter, &mut others);
    let other_inequality = !others.is_empty();
    if key_equality && other_inequality && !key_inequality {
        return Err(Status::invalid_argument(
            "Equality on key is not allowed if there are other inequality fields and key does \
             not appear in inequalities.",
        ));
    }
    Ok(())
}

/// A filter on `__name__`, last (#262): no array membership, no unary filter, and every value
/// a key of `database` ([`check_key`]).
fn check_key_filter(filter: &Filter, database: &str) -> Result<(), Status> {
    match filter {
        Filter::Compare {
            field: Field::Name,
            op: Op::ArrayContains | Op::ArrayContainsAny,
            ..
        } => Err(Status::invalid_argument("the name __key__ is reserved")),
        Filter::Unary {
            field: Field::Name, ..
        } => Err(key_value_required()),
        Filter::Compare {
            field: Field::Name,
            op,
            value,
        } => key_values(*op, value)
            .iter()
            .try_for_each(|value| check_key(value, database)),
        _ => Ok(()),
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
/// key's equalities; the disjunctions come after [`check_index`] ([`check_disjunctions`]). Its
/// operators were checked first ([`check_operators`]), the rest comes last
/// ([`check_filter_late`]).
fn check_filter(filter: &Filter) -> Result<(), Status> {
    let mut leaves = Vec::new();
    collect_leaves(filter, &mut leaves);
    if array_filters(&normalize(filter)) > 1 {
        return Err(Status::failed_precondition(
            "Only a single array-contains clause is allowed in a query",
        ));
    }
    check_key_equality(filter, &leaves)
}

/// The fields `sum` and `avg` read order the query after its own orders ([`Query::require`]):
/// a query on the name (ordered by it, or with a filter on it other than array membership)
/// needs each of them among its orders or its inequalities' fields, or no index serves it
/// (`tools/oracle/aggregations.py`, #262). A descending key scan is refused before this
/// ([`descending_key_scan`]); a collection group below a document, which the official
/// emulator reads as a range of keys, runs (D088).
fn check_index(
    explicit: &[Order],
    filter: Option<&Filter>,
    fields: &[FieldPath],
) -> Result<(), Status> {
    if fields.is_empty() {
        return Ok(());
    }
    let key_order = explicit.iter().any(|o| o.field == Field::Name);
    // Array membership on `__name__` is a field's (`check_key_filter`), not the key's.
    let key_filter = filter.is_some_and(|filter| {
        let mut leaves = Vec::new();
        collect_leaves(filter, &mut leaves);
        leaves.iter().any(|(field, kind)| {
            **field == Field::Name
                && !matches!(
                    kind,
                    Leaf::Compare(Op::ArrayContains | Op::ArrayContainsAny)
                )
        })
    });
    if !(key_order || key_filter) || descending_key_scan(explicit, filter) {
        return Ok(());
    }
    // A field the query orders by, wherever (after the key is refused later), or an
    // inequality's.
    let mut ordered: Vec<Field> = explicit.iter().map(|o| o.field.clone()).collect();
    if let Some(filter) = filter {
        inequality_fields(filter, &mut ordered);
    }
    let mut missing: Vec<&FieldPath> = fields
        .iter()
        .filter(|path| !ordered.contains(&Field::Path((*path).clone())))
        .collect();
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        return Ok(());
    }
    Err(Status::invalid_argument(format!(
        "This query requires an index that has fields [{}] after __name__ and Firestore does \
         not currently support such an index. Please try the query again with additional \
         ordering on those fields.",
        missing
            .iter()
            .map(|path| property_path::shown(path))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// The disjunctions of the filter, normalized: at most 30.
fn check_disjunctions(filter: &Filter) -> Result<(), Status> {
    let normalized = normalize(filter);
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

/// The most filters, orders and ancestors a query may have.
const MAX_TERMS: usize = 100;

/// The filters of every disjunction (as the official emulator counts them: [`Atom::weight`]),
/// the orders that complete the query's (with the fields a `sum` or an `avg` reads), and one
/// for a parent document: at most 100 (`tools/oracle/queries.py`, #262).
fn check_terms(
    filter: Option<&Filter>,
    orders: &[Order],
    explicit: &[Order],
    aggregated: &[FieldPath],
    ancestor: bool,
) -> Result<(), Status> {
    let normalized = filter.map(normalize);
    let filters: usize = normalized.as_ref().and_then(disjunctions).map_or(0, |dnf| {
        dnf.iter()
            .map(|conjunction| conjunction.iter().map(Atom::weight).sum::<usize>())
            .sum()
    });
    let mut sorts = orders.len();
    if !explicit.iter().any(|o| o.field == Field::Name) {
        let mut added: Vec<&FieldPath> = aggregated
            .iter()
            .filter(|path| {
                !orders
                    .iter()
                    .any(|o| o.field == Field::Path((*path).clone()))
            })
            .collect();
        added.sort();
        added.dedup();
        sorts += added.len();
    }
    if filters + sorts + usize::from(ancestor) <= MAX_TERMS {
        return Ok(());
    }
    Err(Status::invalid_argument(format!(
        "The query may not have more than {MAX_TERMS} filters + sort orders + ancestor total. \
         Currently there are {filters} filters, {sorts} sort orders, and {}.",
        if ancestor {
            "one ancestor filter"
        } else {
            "no ancestor filter"
        }
    )))
}

/// The last checks of a filter, after the orders, the limit and whether the orders need a
/// collection (`tools/oracle/queries.py`, #262), filter by filter in the order of its
/// disjunctive normal form ([`dnf_leaves`]): what its path may hold ([`Field::check`]), the
/// size of its values ([`check_value_size`]), whether it needs a collection, then, on
/// `__name__`, its operator and its values ([`check_key_filter`]).
fn check_filter_late(filter: &Filter, kindless: bool, database: &str) -> Result<(), Status> {
    let normalized = normalize(filter);
    for leaf in dnf_leaves(&normalized).into_iter().flatten() {
        let field = match leaf {
            Filter::Compare { field, .. } | Filter::Unary { field, .. } => field,
            Filter::And(_) | Filter::Or(_) => continue,
        };
        field.check()?;
        check_value_size(leaf)?;
        if kindless && *field != Field::Name {
            return Err(Status::invalid_argument(format!(
                "kind is required for filter: {}",
                field.display()
            )));
        }
        check_key_filter(leaf, database)?;
    }
    Ok(())
}

/// A field's value, or each of an `in`, `not-in` or `array-contains-any`, of at most 1500 bytes
/// ([`validate::too_large_for_a_query`]); array membership names the field's elements.
fn check_value_size(leaf: &Filter) -> Result<(), Status> {
    let Filter::Compare {
        field: field @ Field::Path(_),
        op,
        value,
    } = leaf
    else {
        return Ok(());
    };
    let values = match (op, &value.value_type) {
        (Op::In | Op::NotIn | Op::ArrayContainsAny, Some(ValueType::ArrayValue(array))) => {
            array.values.as_slice()
        }
        _ => std::slice::from_ref(value),
    };
    if !values.iter().any(validate::too_large_for_a_query) {
        return Ok(());
    }
    let elements = if matches!(op, Op::ArrayContains | Op::ArrayContainsAny) {
        "[*]"
    } else {
        ""
    };
    Err(Status::invalid_argument(format!(
        "value for {}{elements} is too large to be used in a query",
        field.display()
    )))
}

/// The leaf filters of a normalized filter, conjunction by conjunction of its disjunctive
/// normal form, a leaf once in each conjunction it is part of (`AND(OR(a, b), c)` is `a, c`,
/// then `b, c`). Past [`MAX_DNF_TERMS`] conjunctions, the leaves in the filter's own order.
fn dnf_leaves(filter: &Filter) -> Vec<Vec<&Filter>> {
    fn expand<'a>(filter: &'a Filter) -> Option<Vec<Vec<&'a Filter>>> {
        let terms = match filter {
            Filter::Or(children) => children
                .iter()
                .map(expand)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect(),
            Filter::And(children) => {
                let mut terms = vec![Vec::new()];
                for child in children {
                    let child_terms = expand(child)?;
                    terms = terms
                        .iter()
                        .flat_map(|term: &Vec<&'a Filter>| {
                            child_terms
                                .iter()
                                .map(move |more| term.iter().chain(more).copied().collect())
                        })
                        .collect();
                    if terms.len() > MAX_DNF_TERMS {
                        return None;
                    }
                }
                terms
            }
            leaf => vec![vec![leaf]],
        };
        (terms.len() <= MAX_DNF_TERMS).then_some(terms)
    }
    expand(filter).unwrap_or_else(|| {
        let mut leaves = Vec::new();
        collect_leaf_filters(filter, &mut leaves);
        vec![leaves]
    })
}

/// Bounds [`dnf_leaves`]: more than the disjunctions a query may have, which are counted as
/// sets ([`check_disjunctions`]) before.
const MAX_DNF_TERMS: usize = 1024;

fn collect_leaf_filters<'a>(filter: &'a Filter, out: &mut Vec<&'a Filter>) {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                collect_leaf_filters(filter, out);
            }
        }
        leaf => out.push(leaf),
    }
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

/// What the official emulator counts and compares an atom as (`tools/oracle/queries.py`,
/// #262).
#[derive(PartialEq, Eq)]
enum Class {
    /// An equality, a range or `!=` on null, or an equality or a range on NaN: the same
    /// filter on a field, whatever the operator and the value, which counts twice.
    Nullish,
    /// `array-contains` null or NaN: the same filter on a field.
    ContainsNullish,
    /// Any other, its operator and value its own.
    Plain,
}

impl Atom<'_> {
    fn class(&self) -> Class {
        let Some(value) = self.value.filter(|v| is_null(v) || is_nan(v)) else {
            return Class::Plain;
        };
        match self.kind {
            Leaf::Compare(
                Op::Equal | Op::Less | Op::LessOrEqual | Op::Greater | Op::GreaterOrEqual,
            ) => Class::Nullish,
            Leaf::Compare(Op::NotEqual) if is_null(value) => Class::Nullish,
            Leaf::Compare(Op::ArrayContains) => Class::ContainsNullish,
            _ => Class::Plain,
        }
    }

    fn same(&self, other: &Self) -> bool {
        if self.field != other.field {
            return false;
        }
        match (self.class(), other.class()) {
            (Class::Plain, Class::Plain) => {
                self.kind == other.kind
                    && match (self.value, other.value) {
                        (Some(a), Some(b)) => identical(a, b),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (a, b) => a == b,
        }
    }

    /// How many filters it counts as toward the most a query may have ([`check_terms`]):
    /// a `not-in` one per value, each null twice.
    fn weight(&self) -> usize {
        match (self.kind, self.value) {
            (Leaf::Compare(Op::NotIn), Some(value)) => {
                let mut seen: Vec<&Value> = Vec::new();
                for v in array(value) {
                    if !seen.iter().any(|w| identical(v, w)) {
                        seen.push(v);
                    }
                }
                seen.iter().map(|v| if is_null(v) { 2 } else { 1 }).sum()
            }
            _ if self.class() == Class::Nullish => 2,
            _ => 1,
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
/// `is not NaN` filters, anywhere in the filter (paths with array offsets too, until they are
/// refused), but not of one that matches nothing ([`matches_nothing`]).
fn inequality_fields(filter: &Filter, out: &mut Vec<Field>) {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                inequality_fields(filter, out);
            }
        }
        Filter::Compare {
            field: field @ (Field::Path(_) | Field::Indexed(_)),
            op:
                op @ (Op::Less
                | Op::LessOrEqual
                | Op::Greater
                | Op::GreaterOrEqual
                | Op::NotEqual
                | Op::NotIn),
            value,
        } if !matches_nothing(*op, value) => out.push(field.clone()),
        Filter::Unary {
            field: field @ (Field::Path(_) | Field::Indexed(_)),
            op: UnaryOp::NotNan | UnaryOp::NotNull,
        } => out.push(field.clone()),
        Filter::Compare { .. } | Filter::Unary { .. } => {}
    }
}

/// A range bound that is null or NaN, a `!=` null, or a `not-in` of nulls alone: the official
/// emulator orders by no field of such a filter, so an order by the key alone stands
/// (`tools/oracle/queries.py`, #262).
fn matches_nothing(op: Op, value: &Value) -> bool {
    match op {
        Op::NotEqual => is_null(value),
        Op::NotIn => matches!(
            &value.value_type,
            Some(ValueType::ArrayValue(array)) if !array.values.is_empty() && array.values.iter().all(is_null)
        ),
        _ => is_null(value) || is_nan(value),
    }
}

fn more_fields_after_the_key() -> Status {
    Status::invalid_argument("order by clause cannot contain more fields after the key")
}

/// The explicit ordering, then the inequality fields it lacks (in field path order), then
/// `__name__`; added orderings take the direction of the last explicit one.
fn complete_orders(explicit: &[Order], filter: Option<&Filter>) -> Result<Vec<Order>, Status> {
    // A descending key scan comes first, then the key before other fields, then duplicates
    // (`__name__` twice is the key before another field, but descending only, a descending
    // key scan); whether the orders need a collection is the caller's, after the limit
    // (`tools/oracle/queries.py`, #262).
    if descending_key_scan(explicit, filter) {
        return Err(Status::failed_precondition(
            "Firestore does not support descending key scans",
        ));
    }
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
    // In field path order; paths with array offsets after the others.
    implicit.sort_by(|a, b| match (a, b) {
        (Field::Path(a), Field::Path(b)) => a.cmp(b),
        (Field::Path(_), _) => Ordering::Less,
        (_, Field::Path(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    });
    implicit.dedup();
    for field in implicit {
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
    Ok(orders)
}

/// Orders by the key alone, descending (once or more), with no filter but on the key: the
/// official emulator scans the key range for these, and cannot do so backwards.
fn descending_key_scan(explicit: &[Order], filter: Option<&Filter>) -> bool {
    !explicit.is_empty()
        && explicit
            .iter()
            .all(|o| o.field == Field::Name && o.descending)
        && !filter.is_some_and(filters_a_property)
}

/// Whether `filter` holds a filter on a field: array membership on `__name__` is one too, a
/// field's (`check_key_filter`), not the key's.
fn filters_a_property(filter: &Filter) -> bool {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => filters.iter().any(filters_a_property),
        Filter::Compare {
            field: Field::Name,
            op,
            ..
        } => matches!(op, Op::ArrayContains | Op::ArrayContainsAny),
        Filter::Compare { .. } => true,
        Filter::Unary { field, .. } => *field != Field::Name,
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
            validate::query_value(value)?;
            if order.field == Field::Name
                && !matches!(value.value_type, Some(ValueType::ReferenceValue(_)))
            {
                return Err(Status::invalid_argument(
                    "Cursor __key__ value is not a document reference.",
                ));
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
        Field::Indexed(path) => Rules::Field {
            field: vec![path.to_string()],
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

#[cfg(test)]
mod tests {
    use std::{
        ops::ControlFlow,
        sync::atomic::{AtomicUsize, Ordering as Atomic},
    };

    use hidane_core::store::{
        Commit, ListItem, ListOptions, MemoryStore, StoreError, Visit, WriteBatch,
    };
    use hidane_proto::google::firestore::v1::structured_query::{
        CollectionSelector, Direction, FieldReference, Order as QueryOrder,
    };

    use super::*;

    const DB: &str = "projects/demo/databases/(default)";

    /// A store that counts the documents its scans visit.
    struct Counting {
        inner: MemoryStore,
        visited: AtomicUsize,
    }

    impl Store for Counting {
        fn latest_read_time(&self, database: &str) -> ReadTime {
            self.inner.latest_read_time(database)
        }
        fn earliest_read_time(&self, database: &str) -> ReadTime {
            self.inner.earliest_read_time(database)
        }
        fn get(
            &self,
            database: &str,
            path: &ResourcePath,
            at: ReadTime,
        ) -> Option<Arc<StoredDocument>> {
            self.inner.get(database, path, at)
        }
        fn list_collection(
            &self,
            database: &str,
            collection: &ResourcePath,
            at: ReadTime,
            options: ListOptions<'_>,
            visit: &mut dyn FnMut(ListItem<'_>) -> ControlFlow<()>,
        ) {
            self.inner
                .list_collection(database, collection, at, options, &mut |item| {
                    self.visited.fetch_add(1, Atomic::Relaxed);
                    visit(item)
                });
        }
        fn scan_collection_group(
            &self,
            database: &str,
            parent: &ResourcePath,
            collection_id: &str,
            at: ReadTime,
            visit: &mut Visit<'_>,
        ) {
            self.inner
                .scan_collection_group(database, parent, collection_id, at, &mut |doc| {
                    self.visited.fetch_add(1, Atomic::Relaxed);
                    visit(doc)
                });
        }
        fn scan_descendants(
            &self,
            database: &str,
            parent: &ResourcePath,
            at: ReadTime,
            visit: &mut Visit<'_>,
        ) {
            self.inner
                .scan_descendants(database, parent, at, &mut |doc| {
                    self.visited.fetch_add(1, Atomic::Relaxed);
                    visit(doc)
                });
        }
        fn list_collection_ids(
            &self,
            database: &str,
            parent: &ResourcePath,
            at: ReadTime,
        ) -> Vec<String> {
            self.inner.list_collection_ids(database, parent, at)
        }
        fn commit(
            &self,
            database: &str,
            write: &mut dyn FnMut(&mut dyn WriteBatch) -> Result<(), StoreError>,
        ) -> Result<Commit, StoreError> {
            self.inner.commit(database, write)
        }
        fn clear_database(&self, database: &str) -> ReadTime {
            self.inner.clear_database(database)
        }
        fn database_names(&self) -> Vec<String> {
            self.inner.database_names()
        }
        fn clear(&self) {
            self.inner.clear()
        }
    }

    /// `c/d000` … `c/d099`, with `n` counting down from 99, and `g/d000/c/x` in the same group.
    fn store() -> Counting {
        let store = Counting {
            inner: MemoryStore::new(),
            visited: AtomicUsize::new(0),
        };
        store
            .commit(DB, &mut |batch| {
                for i in 0..100 {
                    let n = Value {
                        value_type: Some(ValueType::IntegerValue(99 - i)),
                    };
                    batch.set(
                        &ResourcePath::parse(&format!("c/d{i:03}")).unwrap(),
                        [("n".to_owned(), n)].into(),
                    );
                }
                batch.set(
                    &ResourcePath::parse("g/d000/c/x").unwrap(),
                    Default::default(),
                );
                Ok(())
            })
            .unwrap();
        store
    }

    fn query(
        group: bool,
        order: Option<(&str, bool)>,
        offset: i32,
        limit: Option<i32>,
    ) -> StructuredQuery {
        StructuredQuery {
            from: vec![CollectionSelector {
                collection_id: "c".to_owned(),
                all_descendants: group,
            }],
            order_by: order
                .map(|(field, descending)| QueryOrder {
                    field: Some(FieldReference {
                        field_path: field.to_owned(),
                    }),
                    direction: if descending {
                        Direction::Descending
                    } else {
                        Direction::Ascending
                    } as i32,
                })
                .into_iter()
                .collect(),
            offset,
            limit,
            ..StructuredQuery::default()
        }
    }

    /// The documents' paths, and how many documents the scans visited.
    fn run(store: &Counting, query: &StructuredQuery) -> (Vec<String>, usize) {
        let query = Query::parse(DB, ResourcePath::root(), query).unwrap();
        store.visited.store(0, Atomic::Relaxed);
        let at = store.latest_read_time(DB);
        let results = query.run(store, DB, at);
        let paths = results
            .documents
            .iter()
            .map(|d| d.path.to_string())
            .collect();
        (paths, store.visited.load(Atomic::Relaxed))
    }

    /// #295: a limit and an offset keep what a full sort keeps, for every order, and a query
    /// ordered by name alone stops scanning once it has its documents.
    #[test]
    fn a_limit_keeps_what_a_full_sort_keeps_and_stops_early_by_name() {
        let store = store();
        for group in [false, true] {
            // A descending key scan is refused, as on the official emulator.
            for order in [
                None,
                Some(("__name__", false)),
                Some(("n", false)),
                Some(("n", true)),
            ] {
                let (all, scanned) = run(&store, &query(group, order, 0, None));
                assert_eq!(scanned, if group { 101 } else { 100 });
                for (offset, limit) in [(0, 0), (0, 1), (0, 3), (2, 3), (98, 5), (0, 200)] {
                    let (some, visited) = run(&store, &query(group, order, offset, Some(limit)));
                    let expected: Vec<String> = all
                        .iter()
                        .skip(offset as usize)
                        .take(limit as usize)
                        .cloned()
                        .collect();
                    let what = format!("group {group}, {order:?}, offset {offset}, limit {limit}");
                    assert_eq!(some, expected, "{what}");
                    if matches!(order, None | Some(("__name__", false))) {
                        let wanted = (offset + limit) as usize;
                        assert_eq!(visited, wanted.clamp(1, scanned), "{what}");
                    } else {
                        assert_eq!(visited, scanned, "{what}");
                    }
                }
            }
        }
    }
}
