//! The checks the official emulator makes of a Datastore query before it runs it, in its order
//! (`tests/fixtures/datastore_checks.json`, from `tools/oracle/datastore_checks.py`, which pairs
//! every two problems and records which one the official emulator reports).
//!
//! The query of a `RunQuery` and the query an aggregation of `RunAggregationQuery` runs over
//! go through different checks, worded differently. An aggregation's query accepts what a
//! query refuses (an ancestor in a disjunction, `NOT_IN` with `IN` or `OR`, more than one
//! `NOT_EQUAL`, entity operands), and an array inside an operand there is an internal error.
//! A filter's operand is checked as a written value is ([`super::values`]) in a `RunQuery`; an
//! aggregation's query reads its operands otherwise, in two steps. Some checks come before the
//! query's index is looked up (`--require_indexes`) and the others after it: a check answers the
//! first early refusal as its error, and the first late one, if any, as its value.

use hidane_proto::google::datastore::v1::{
    AggregationQuery, Filter, Key, PropertyFilter, Query, Value,
    aggregation_query::aggregation::Operator as Aggregation, composite_filter, filter::FilterType,
    property_filter::Operator, property_order::Direction, value::ValueType,
};
use tonic::Status;

use super::keys::{Database, Id, Located, entity_ref, reserved};
use super::query::{MAX_IN, MAX_NOT_IN, cursor_parses, cursors_match};
use super::values::{
    check_aggregation_operand, check_aggregation_operand_late, check_filter_operand, is_vector,
};

/// The first refusal that comes once the query's index is found, or the first before it.
pub type Checked = Result<Option<Status>, Status>;

/// The most aggregations one query may have.
pub const MAX_AGGREGATIONS: usize = 5;

/// An `IN` or `NOT_IN` filter's element that is an array without values, or unindexed: an
/// internal error on the official emulator, once everything else is checked.
fn unreadable_elements(all: &[(&Filter, bool)]) -> Result<(), Status> {
    for (filter, _) in properties(all) {
        if matches!(operator(filter), Some(Operator::In | Operator::NotIn))
            && let Some(ValueType::ArrayValue(array)) = value(filter)
            && array.values.iter().any(|e| {
                empty(e)
                    || (e.exclude_from_indexes
                        && !matches!(e.value_type, Some(ValueType::ArrayValue(_))))
            })
        {
            return Err(Status::unknown(""));
        }
    }
    Ok(())
}

/// Whether an array value has no values.
fn empty(value: &hidane_proto::google::datastore::v1::Value) -> bool {
    matches!(&value.value_type, Some(ValueType::ArrayValue(a)) if a.values.is_empty())
}

/// The longest property name a filter may have, in bytes.
const MAX_NAME: usize = 1500;

/// The most disjunctions a query's filter may make in disjunctive normal form.
const MAX_DISJUNCTIONS: usize = 30;

const UNIQUE_ANCESTOR: &str = "All disjunctions must have a common and unique ancestor constraint.";
const KINDLESS_ORDER: &str = "kind is required for all orders except __key__ ascending";
const RESERVED_KINDS: &str = "Aggregation queries on reserved kinds are not supported.";
const EMPTY_SEGMENT: &str = "a property path segment cannot be empty";

fn invalid(message: impl Into<String>) -> Status {
    Status::invalid_argument(message)
}

/// A query's filters, depth first, each with whether a disjunction holds it.
fn filters(filter: Option<&Filter>) -> Vec<(&Filter, bool)> {
    fn walk<'a>(filter: &'a Filter, in_or: bool, out: &mut Vec<(&'a Filter, bool)>) {
        out.push((filter, in_or));
        if let Some(FilterType::CompositeFilter(composite)) = &filter.filter_type {
            let or = in_or || composite.op == composite_filter::Operator::Or as i32;
            for child in &composite.filters {
                walk(child, or, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(filter) = filter {
        walk(filter, false, &mut out);
    }
    out
}

/// Whether a filter is a `HAS_ANCESTOR` filter, which the official emulator takes out of the
/// filter as the query's ancestor before it normalizes the rest.
fn bare_ancestor(filter: &Filter) -> bool {
    matches!(&filter.filter_type,
        Some(FilterType::PropertyFilter(p)) if operator(p) == Some(Operator::HasAncestor))
}

/// A filter in disjunctive normal form as the official emulator normalizes it: without its
/// `HAS_ANCESTOR` filters, an `IN` an equality per value, each conjunction once (as a set) and
/// each filter once in it. `None` once a conjunction would make more than twice the most
/// disjunctions allowed: the official emulator stops counting there.
fn normalized(filter: &Filter) -> Option<Vec<Vec<PropertyFilter>>> {
    fn add(out: &mut Vec<Vec<PropertyFilter>>, conjunction: Vec<PropertyFilter>) {
        let same = |seen: &Vec<PropertyFilter>| {
            seen.len() == conjunction.len() && seen.iter().all(|f| conjunction.contains(f))
        };
        if !out.iter().any(same) {
            out.push(conjunction);
        }
    }
    let mut out = Vec::new();
    match &filter.filter_type {
        _ if bare_ancestor(filter) => out.push(Vec::new()),
        Some(FilterType::CompositeFilter(composite))
            if composite.op == composite_filter::Operator::Or as i32 =>
        {
            for child in composite.filters.iter().filter(|f| !bare_ancestor(f)) {
                for conjunction in normalized(child)? {
                    add(&mut out, conjunction);
                }
            }
        }
        Some(FilterType::CompositeFilter(composite)) => {
            let children: Vec<&Filter> = composite
                .filters
                .iter()
                .filter(|f| !bare_ancestor(f))
                .collect();
            let Some((first, rest)) = children.split_first() else {
                return Some(vec![Vec::new()]);
            };
            out = normalized(first)?;
            for child in rest {
                let terms = normalized(child)?;
                if out.len() * terms.len() > 2 * MAX_DISJUNCTIONS {
                    return None;
                }
                let mut product = Vec::new();
                for left in &out {
                    for right in &terms {
                        let mut conjunction: Vec<PropertyFilter> = Vec::new();
                        for f in left.iter().chain(right) {
                            if !conjunction.contains(f) {
                                conjunction.push(f.clone());
                            }
                        }
                        add(&mut product, conjunction);
                    }
                }
                out = product;
            }
        }
        Some(FilterType::PropertyFilter(property)) => match (operator(property), value(property)) {
            (Some(Operator::In), Some(ValueType::ArrayValue(array))) => {
                for v in &array.values {
                    add(
                        &mut out,
                        vec![PropertyFilter {
                            property: property.property.clone(),
                            op: Operator::Equal as i32,
                            value: Some(v.clone()),
                        }],
                    );
                }
            }
            _ => out.push(vec![property.clone()]),
        },
        None => out.push(Vec::new()),
    }
    Some(out)
}

/// At most 30 disjunctions in disjunctive normal form.
fn disjunctions(query: &Query) -> Result<(), Status> {
    let Some(filter) = &query.filter else {
        return Ok(());
    };
    let Some(conjunctions) = normalized(filter) else {
        return Err(invalid(format!(
            "Too many disjunctions after normalization, the maximum is {MAX_DISJUNCTIONS}"
        )));
    };
    if conjunctions.len() > MAX_DISJUNCTIONS {
        return Err(invalid(format!(
            "Too many disjunctions after normalization. Result had {} disjunctions which is more than the maximum of {MAX_DISJUNCTIONS}",
            conjunctions.len()
        )));
    }
    Ok(())
}

/// The property filters of a query's filter in disjunctive normal form, conjunction by
/// conjunction, as the checks made once its index is found walk them.
fn normalized_filters(query: &Query) -> Vec<PropertyFilter> {
    query
        .filter
        .as_ref()
        .and_then(normalized)
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .collect()
}

/// The property filters among `all`, in order.
fn properties<'a>(all: &[(&'a Filter, bool)]) -> impl Iterator<Item = (&'a PropertyFilter, bool)> {
    all.iter()
        .filter_map(|(filter, in_or)| match &filter.filter_type {
            Some(FilterType::PropertyFilter(property)) => Some((property, *in_or)),
            _ => None,
        })
}

fn has_or(all: &[(&Filter, bool)]) -> bool {
    all.iter().any(|(filter, _)| {
        matches!(&filter.filter_type, Some(FilterType::CompositeFilter(c))
            if c.op == composite_filter::Operator::Or as i32)
    })
}

fn name(filter: &PropertyFilter) -> &str {
    filter.property.as_ref().map_or("", |p| p.name.as_str())
}

fn operator(filter: &PropertyFilter) -> Option<Operator> {
    Operator::try_from(filter.op)
        .ok()
        .filter(|op| *op != Operator::Unspecified)
}

fn value(filter: &PropertyFilter) -> Option<&ValueType> {
    filter.value.as_ref().and_then(|v| v.value_type.as_ref())
}

/// What a filter compares with: the elements of an `IN` or `NOT_IN` array, or its value.
fn operands(filter: &PropertyFilter) -> Vec<&ValueType> {
    match (operator(filter), value(filter)) {
        (Some(Operator::In | Operator::NotIn), Some(ValueType::ArrayValue(array))) => array
            .values
            .iter()
            .filter_map(|v| v.value_type.as_ref())
            .collect(),
        (_, Some(v)) => vec![v],
        (_, None) => Vec::new(),
    }
}

fn inequality(op: Option<Operator>) -> bool {
    matches!(
        op,
        Some(
            Operator::LessThan
                | Operator::LessThanOrEqual
                | Operator::GreaterThan
                | Operator::GreaterThanOrEqual
                | Operator::NotEqual
                | Operator::NotIn
        )
    )
}

fn key_namespace(key: &Key) -> &str {
    key.partition_id
        .as_ref()
        .map_or("", |p| p.namespace_id.as_str())
}

/// A key as `EntityRef[…]` prints it, in `database`.
fn shown(database: &Database, key: &Key) -> String {
    let path = key
        .path
        .iter()
        .map(|element| {
            let id = match &element.id_type {
                Some(hidane_proto::google::datastore::v1::key::path_element::IdType::Id(n)) => {
                    Id::Id(*n)
                }
                Some(hidane_proto::google::datastore::v1::key::path_element::IdType::Name(n)) => {
                    Id::Name(n.clone())
                }
                None => Id::Name(String::new()),
            };
            (element.kind.clone(), id)
        })
        .collect();
    entity_ref(
        database,
        &Located {
            namespace: key_namespace(key).to_owned(),
            path,
        },
    )
}

fn names(query: &Query) -> impl Iterator<Item = Option<&str>> {
    let projected = query
        .projection
        .iter()
        .map(|p| p.property.as_ref().map(|p| p.name.as_str()));
    let ordered = query
        .order
        .iter()
        .map(|o| o.property.as_ref().map(|p| p.name.as_str()));
    let distinct = query.distinct_on.iter().map(|p| Some(p.name.as_str()));
    projected.chain(ordered).chain(distinct)
}

fn duplicate_order(query: &Query) -> Result<(), Status> {
    let mut seen: Vec<&str> = Vec::new();
    for order in &query.order {
        let name = order.property.as_ref().map_or("", |p| p.name.as_str());
        if seen.contains(&name) {
            return Err(invalid(format!(
                "order by clause cannot contain duplicate fields {name}"
            )));
        }
        seen.push(name);
    }
    Ok(())
}

fn cursors(query: &Query) -> Result<(), Status> {
    for cursor in [&query.start_cursor, &query.end_cursor] {
        if !cursor.is_empty() && !cursor_parses(cursor) {
            return Err(invalid("Error parsing protocol message"));
        }
    }
    Ok(())
}

/// Whether, without a kind, the query orders by more than its key ascending: its orders, then
/// the properties it is distinct on, has an inequality on or projects.
fn ordered_beyond_key(query: &Query, all: &[(&Filter, bool)]) -> bool {
    let explicit = query.order.iter().any(|o| {
        o.property.as_ref().is_none_or(|p| p.name != "__key__")
            || o.direction == Direction::Descending as i32
    });
    let distinct = query.distinct_on.iter().any(|p| p.name != "__key__");
    let unequal = properties(all).any(|(f, _)| inequality(operator(f)) && name(f) != "__key__");
    let projected = query
        .projection
        .iter()
        .any(|p| p.property.as_ref().is_none_or(|p| p.name != "__key__"));
    explicit || distinct || unequal || projected
}

/// The projection checks: distinct on the key, a reserved name projected, a property projected
/// twice (the key may be), distinct on a reserved name.
fn projection(query: &Query) -> Result<(), Status> {
    if query.distinct_on.iter().any(|p| p.name == "__key__") {
        return Err(invalid(
            "Group by is not supported for the property: __key__",
        ));
    }
    let mut projected: Vec<&str> = Vec::new();
    for p in &query.projection {
        let name = p.property.as_ref().map_or("", |p| p.name.as_str());
        if name == "__key__" {
            continue;
        }
        if reserved(name) {
            return Err(invalid(format!(
                "projections are not supported for the property: {name}"
            )));
        }
        if projected.contains(&name) {
            return Err(invalid("Cannot project a property multiple times."));
        }
        projected.push(name);
    }
    if let Some(p) = query.distinct_on.iter().find(|p| reserved(&p.name)) {
        return Err(invalid(format!(
            "Group by is not supported for the property: {}",
            p.name
        )));
    }
    Ok(())
}

/// A filter by equality on a projected property.
fn projected_equality(query: &Query, filter: &PropertyFilter) -> Result<(), Status> {
    if matches!(operator(filter), Some(Operator::Equal | Operator::In))
        && query
            .projection
            .iter()
            .any(|p| p.property.as_ref().is_some_and(|p| p.name == name(filter)))
    {
        return Err(invalid(
            "Cannot use projection on a property with an equality filter.",
        ));
    }
    Ok(())
}

/// A filter by equality on a property the query is distinct on.
fn grouped_equality(query: &Query, filter: &PropertyFilter) -> Result<(), Status> {
    if matches!(operator(filter), Some(Operator::Equal | Operator::In))
        && query.distinct_on.iter().any(|p| p.name == name(filter))
    {
        return Err(invalid(
            "Cannot use group by on a property with an equality filter.",
        ));
    }
    Ok(())
}

/// The properties a query is distinct on come first among its orders.
fn grouped_first(query: &Query) -> Result<(), Status> {
    let distinct: Vec<&str> = query.distinct_on.iter().map(|p| p.name.as_str()).collect();
    if distinct.is_empty() {
        return Ok(());
    }
    let mut grouped: Vec<&str> = query
        .order
        .iter()
        .map(|o| o.property.as_ref().map_or("", |p| p.name.as_str()))
        .collect();
    for name in &distinct {
        if !grouped.contains(name) {
            grouped.push(name);
        }
    }
    if let Some(other) = grouped.iter().position(|n| !distinct.contains(n))
        && grouped[other..].iter().any(|n| distinct.contains(n))
    {
        return Err(invalid(
            "All group by properties must be ordered before any non group by properties.",
        ));
    }
    Ok(())
}

/// The checks of `RunQuery`'s query, in `namespace` of `database`; `read_time` the verdict on
/// the request's read time, which comes after the cursors.
pub fn entity_query(
    query: &Query,
    namespace: &str,
    database: &Database,
    read_time: Result<(), Status>,
) -> Checked {
    let all = filters(query.filter.as_ref());
    for (filter, _) in &all {
        entity_shape(filter)?;
    }
    if let Some(filter) = &query.filter {
        common_ancestor(filter).map_err(|()| invalid(UNIQUE_ANCESTOR))?;
    }
    if query.kind.iter().any(|k| k.name.is_empty()) {
        return Err(invalid("The kind is the empty string."));
    }
    if names(query).any(|n| n.is_none_or(str::is_empty)) {
        return Err(invalid("The property.name is the empty string."));
    }
    if query.limit.is_some_and(|n| n < 0) {
        return Err(invalid("Limit must be non-negative."));
    }
    if query.offset < 0 {
        return Err(invalid("Offset must be non-negative."));
    }
    let negations = properties(&all)
        .filter(|(f, _)| matches!(operator(f), Some(Operator::NotEqual | Operator::NotIn)))
        .count();
    if negations > 1 {
        return Err(invalid(
            "Only a single 'NOT_EQUAL' or 'NOT_IN' is allowed per query.",
        ));
    }
    let not_in = properties(&all).any(|(f, _)| operator(f) == Some(Operator::NotIn));
    let any_in = properties(&all).any(|(f, _)| operator(f) == Some(Operator::In));
    if not_in && (any_in || has_or(&all)) {
        return Err(invalid(
            "'NOT_IN' cannot be used in the same query with 'IN' or 'OR'.",
        ));
    }
    if query.kind.len() > 1 {
        return Err(invalid("multiple kinds not supported"));
    }
    ancestors_alone(&all)?;
    for (filter, _) in properties(&all) {
        operand_shape(filter)?;
    }
    cursors(query)?;
    read_time?;
    disjunctions(query)?;
    duplicate_order(query)?;
    Ok(entity_late(query, namespace, database, &all).err())
}

/// The ancestor a filter holds to: its own (`HAS_ANCESTOR`), the one of a conjunction's
/// filters that have one, or the one every filter of a disjunction holds to. `Err` when the
/// filters of a conjunction hold to different ones, or those of a disjunction do not all hold
/// to the same one (or all to none).
fn common_ancestor(filter: &Filter) -> Result<Option<&Value>, ()> {
    match &filter.filter_type {
        Some(FilterType::PropertyFilter(property)) => Ok((operator(property)
            == Some(Operator::HasAncestor))
        .then_some(property.value.as_ref())
        .flatten()),
        Some(FilterType::CompositeFilter(composite)) => {
            let ancestors = composite
                .filters
                .iter()
                .map(common_ancestor)
                .collect::<Result<Vec<_>, ()>>()?;
            if composite.op == composite_filter::Operator::Or as i32 {
                match ancestors.split_first() {
                    Some((first, rest)) if rest.iter().all(|a| a == first) => Ok(*first),
                    Some(_) => Err(()),
                    None => Ok(None),
                }
            } else {
                let mut found = None;
                for ancestor in ancestors.into_iter().flatten() {
                    if found.is_some_and(|f| f != ancestor) {
                        return Err(());
                    }
                    found = Some(ancestor);
                }
                Ok(found)
            }
        }
        None => Ok(None),
    }
}

/// A disjunction of `HAS_ANCESTOR` filters alone is an internal error on the official emulator:
/// without them ([`normalized`]), it is empty.
fn ancestors_alone(all: &[(&Filter, bool)]) -> Result<(), Status> {
    let alone = all.iter().any(|(filter, _)| {
        matches!(&filter.filter_type, Some(FilterType::CompositeFilter(c))
            if c.op == composite_filter::Operator::Or as i32 && c.filters.iter().all(bare_ancestor))
    });
    if alone {
        return Err(Status::unknown(""));
    }
    Ok(())
}

/// A filter's own parts, as `RunQuery` checks them first.
fn entity_shape(filter: &Filter) -> Result<(), Status> {
    match &filter.filter_type {
        None => Err(invalid("a filter must have exactly one of its fields set")),
        Some(FilterType::CompositeFilter(composite)) => {
            if composite_filter::Operator::try_from(composite.op)
                .is_ok_and(|op| op == composite_filter::Operator::Unspecified)
                || composite_filter::Operator::try_from(composite.op).is_err()
            {
                return Err(invalid("a composite filter must specify an operator"));
            }
            if composite.filters.is_empty() {
                return Err(invalid(
                    "A composite filter must have at least one sub-filter.",
                ));
            }
            Ok(())
        }
        Some(FilterType::PropertyFilter(property)) => {
            let Some(op) = operator(property) else {
                return Err(invalid("a property filter must specify an operator"));
            };
            let name = name(property);
            if name.is_empty() {
                return Err(invalid("The property.name is the empty string."));
            }
            if name.len() > MAX_NAME {
                return Err(invalid(format!(
                    "The property.name is longer than {MAX_NAME} bytes."
                )));
            }
            let Some(value) = value(property) else {
                return Err(invalid(format!(
                    "The value \"{name}\" does not contain a value."
                )));
            };
            let operand = property.value.as_ref().expect("a value");
            if operand.exclude_from_indexes {
                return Err(invalid("a filter value must be indexed"));
            }
            let listed = matches!(op, Operator::In | Operator::NotIn);
            match (listed, value) {
                // A vector is not a list.
                _ if is_vector(operand) && !listed => {
                    return Err(invalid("A vector value is not allowed"));
                }
                (true, ValueType::ArrayValue(array)) if !is_vector(operand) => {
                    // An array without values is left to the end ([`unreadable_elements`]).
                    if array.values.iter().any(|element| {
                        matches!(element.value_type, Some(ValueType::ArrayValue(_)))
                            && !empty(element)
                    }) {
                        return Err(invalid(
                            "list_value cannot contain a Value containing another list_value.",
                        ));
                    }
                }
                (false, ValueType::ArrayValue(_)) => {
                    return Err(invalid("A list value is not allowed"));
                }
                _ => {}
            }
            check_filter_operand(name, operand)?;
            // An entity value that stands for a user, a regular expression or a timestamp of a
            // request is not one.
            if matches!(value, ValueType::EntityValue(_))
                && !matches!(operand.meaning, 20 | 35 | 37)
            {
                return Err(invalid("An entity value is not allowed"));
            }
            Ok(())
        }
    }
}

/// An `IN`, `NOT_IN` or `HAS_ANCESTOR` filter's operand.
fn operand_shape(filter: &PropertyFilter) -> Result<(), Status> {
    match operator(filter) {
        Some(op @ (Operator::In | Operator::NotIn)) => {
            let label = if op == Operator::In { "IN" } else { "NOT_IN" };
            // A vector is not an array here.
            let array = match value(filter) {
                Some(ValueType::ArrayValue(array))
                    if !filter.value.as_ref().is_some_and(is_vector) =>
                {
                    array
                }
                _ => return Err(invalid(format!("{label} requires an ArrayValue."))),
            };
            if array.values.is_empty() {
                return Err(invalid(format!(
                    "{label} requires an non-empty ArrayValue."
                )));
            }
            if op == Operator::In && array.values.len() > MAX_IN {
                return Err(invalid(format!(
                    "IN supports up to {MAX_IN} values. There are {} values in the IN filter in your query.",
                    array.values.len()
                )));
            }
            if op == Operator::NotIn && array.values.len() > MAX_NOT_IN {
                return Err(invalid(format!(
                    "NOT_IN supports up to {MAX_NOT_IN} values."
                )));
            }
            Ok(())
        }
        Some(Operator::HasAncestor) => {
            if !matches!(value(filter), Some(ValueType::KeyValue(_))) {
                return Err(invalid("HAS_ANCESTOR requires a key value"));
            }
            if name(filter) != "__key__" {
                return Err(invalid("property must be __key__"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn ancestor_namespace(all: &[(&Filter, bool)], namespace: &str) -> Result<(), Status> {
    for (filter, _) in properties(all) {
        if operator(filter) == Some(Operator::HasAncestor)
            && let Some(ValueType::KeyValue(key)) = value(filter)
            && key_namespace(key) != namespace
        {
            return Err(invalid(format!(
                "The query namespace is '{namespace}' but ancestor namespace is '{}'.",
                key_namespace(key)
            )));
        }
    }
    Ok(())
}

/// A `__key__` filter's operands: keys, in the query's namespace.
fn key_operands(filter: &PropertyFilter, namespace: &str) -> Result<(), Status> {
    for operand in operands(filter) {
        let ValueType::KeyValue(key) = operand else {
            return Err(invalid("__key__ filter value must be a Key"));
        };
        if key_namespace(key) != namespace {
            return Err(invalid(format!(
                "The query namespace is '{namespace}' but __key__ filter namespace is '{}'.",
                key_namespace(key)
            )));
        }
    }
    Ok(())
}

/// What looking up the index of a query without a kind refuses (`--require_indexes`): orders
/// beyond the key ascending, then a filter on a property. Without index enforcement the same
/// come later, among the checks made once the index is found.
pub fn kindless(query: &Query) -> Result<(), Status> {
    if !query.kind.is_empty() {
        return Ok(());
    }
    let all = filters(query.filter.as_ref());
    if ordered_beyond_key(query, &all) {
        return Err(invalid(KINDLESS_ORDER));
    }
    if let Some((filter, _)) = properties(&all)
        .find(|(f, _)| name(f) != "__key__" && operator(f) != Some(Operator::HasAncestor))
    {
        return Err(invalid(format!(
            "kind is required for filter: {}",
            name(filter)
        )));
    }
    Ok(())
}

/// The checks of `RunQuery`'s query once its index is found.
fn entity_late(
    query: &Query,
    namespace: &str,
    database: &Database,
    all: &[(&Filter, bool)],
) -> Result<(), Status> {
    ancestor_namespace(all, namespace)?;
    if query.kind.is_empty() && ordered_beyond_key(query, all) {
        return Err(invalid(KINDLESS_ORDER));
    }
    projection(query)?;
    for filter in &normalized_filters(query) {
        if operator(filter) == Some(Operator::HasAncestor) {
            continue;
        }
        if name(filter) == "__key__" {
            key_operands(filter, namespace)?;
        }
        projected_equality(query, filter)?;
        grouped_equality(query, filter)?;
        // An `IN` filter's element that is an array without values: an internal error.
        if filter.value.as_ref().is_some_and(empty) {
            return Err(Status::unknown(""));
        }
        if name(filter) != "__key__" && query.kind.is_empty() {
            return Err(invalid(format!(
                "kind is required for filter: {}",
                name(filter)
            )));
        }
    }
    grouped_first(query)?;
    cursors_match(query)?;
    metadata(query, database, all)?;
    unreadable_elements(all)
}

/// The queries of the kinds and of the namespaces: no projection but of the key, the key
/// ascending, and filters only by keys of their own kind, without parents or ancestors.
fn metadata(query: &Query, database: &Database, all: &[(&Filter, bool)]) -> Result<(), Status> {
    let Some(kind @ ("__kind__" | "__namespace__")) = query.kind.first().map(|k| k.name.as_str())
    else {
        return Ok(());
    };
    if properties(all).any(|(f, _)| operator(f) == Some(Operator::HasAncestor)) {
        return Err(invalid(format!("ancestor queries on {kind} not allowed")));
    }
    if !query.projection.is_empty()
        && !query
            .projection
            .iter()
            .all(|p| p.property.as_ref().is_some_and(|p| p.name == "__key__"))
    {
        return Err(invalid(format!(
            "Projections are not supported for the kind: {kind}"
        )));
    }
    // Only the first order counts.
    if query
        .order
        .first()
        .is_some_and(|o| o.direction == Direction::Descending as i32)
    {
        return Err(invalid("Only ascending order on __key__ supported"));
    }
    for (filter, _) in properties(all) {
        if name(filter) != "__key__" {
            return Err(invalid(format!(
                "Only comparison filters on __key__ supported for {kind} queries"
            )));
        }
        for operand in operands(filter) {
            let ValueType::KeyValue(key) = operand else {
                continue;
            };
            if key.path.len() > 1 {
                return Err(invalid(format!(
                    "Key filter on {kind} is invalid (key has parent) - received:\n {}",
                    shown(database, key)
                )));
            }
            if key.path.last().is_none_or(|e| e.kind != kind) {
                return Err(invalid(format!(
                    "Key filter on {kind} is invalid (must be a key for {kind}) - received\n {}",
                    shown(database, key)
                )));
            }
        }
    }
    Ok(())
}

/// Whether an aggregation reads a property: a sum or an average, of its property.
fn aggregated(operator: Option<&Aggregation>) -> Option<&str> {
    match operator {
        Some(Aggregation::Sum(sum)) => Some(sum.property.as_ref().map_or("", |p| p.name.as_str())),
        Some(Aggregation::Avg(avg)) => Some(avg.property.as_ref().map_or("", |p| p.name.as_str())),
        _ => None,
    }
}

/// The checks of `RunAggregationQuery`: its aggregations and the query they run over, in
/// `namespace`.
pub fn aggregation_query(
    aggregation: &AggregationQuery,
    query: &Query,
    namespace: &str,
    read_time: Result<(), Status>,
) -> Checked {
    for a in &aggregation.aggregations {
        match &a.operator {
            None => return Err(invalid("Operator field in Aggregation is not set.")),
            Some(Aggregation::Count(count)) if count.up_to.is_some_and(|n| n < 0) => {
                return Err(invalid(
                    "The `up_to` value in a COUNT aggregation must be greater than or equal to zero.",
                ));
            }
            operator => {
                if aggregated(operator.as_ref()).is_some_and(str::is_empty) {
                    return Err(invalid("Empty aggregation field."));
                }
            }
        }
    }
    if query.kind.len() > 1 {
        return Err(invalid("multiple kinds not supported"));
    }
    let all = filters(query.filter.as_ref());
    let mut ancestors = Vec::new();
    for (filter, _) in &all {
        nested_shape(filter, &mut ancestors)?;
    }
    ancestors_alone(&all)?;
    cursors(query)?;
    read_time?;
    disjunctions(query)?;
    duplicate_order(query)?;
    Ok(aggregation_late(aggregation, query, namespace, &all).err())
}

/// A filter of an aggregation's query, as its first checks see it.
fn nested_shape<'a>(filter: &'a Filter, ancestors: &mut Vec<&'a Value>) -> Result<(), Status> {
    match &filter.filter_type {
        None => Err(invalid("Missing filter operator.")),
        Some(FilterType::CompositeFilter(composite)) => {
            if composite_filter::Operator::try_from(composite.op)
                .is_ok_and(|op| op == composite_filter::Operator::Unspecified)
                || composite_filter::Operator::try_from(composite.op).is_err()
            {
                return Err(invalid("Unsupported CompositeFilter operator."));
            }
            if composite.filters.is_empty() {
                return Err(invalid(
                    "A composite filter must have at least one sub-filter.",
                ));
            }
            Ok(())
        }
        Some(FilterType::PropertyFilter(property)) => {
            if property.property.is_none() {
                return Err(invalid("Filter must specify property reference."));
            }
            let Some(op) = operator(property) else {
                return Err(invalid("Unsupported PropertyFilter operator."));
            };
            if value(property).is_none() {
                return Err(invalid("Value missing type."));
            }
            check_aggregation_operand(property.value.as_ref().expect("a value"))?;
            operand_shape(property)?;
            if op == Operator::HasAncestor
                && let Some(ancestor) = &property.value
                && !ancestors.contains(&ancestor)
            {
                ancestors.push(ancestor);
                if ancestors.len() > 1 {
                    return Err(invalid("duplicate ancestor or parent constraint"));
                }
            }
            Ok(())
        }
    }
}

/// The checks of an aggregation query once its index is found.
fn aggregation_late(
    aggregation: &AggregationQuery,
    query: &Query,
    namespace: &str,
    all: &[(&Filter, bool)],
) -> Result<(), Status> {
    let list = &aggregation.aggregations;
    if list.is_empty() {
        return Err(invalid("Aggregations can not be empty."));
    }
    if list.len() > MAX_AGGREGATIONS {
        return Err(invalid(format!(
            "The maximum number of aggregations allowed in an aggregation query is {MAX_AGGREGATIONS}. Received: {}",
            list.len()
        )));
    }
    for a in list {
        if reserved(&a.alias) {
            return Err(invalid(format!(
                "The property.name \"{}\" is reserved.",
                a.alias
            )));
        }
    }
    if query.kind.first().is_some_and(|k| reserved(&k.name)) {
        return Err(invalid(RESERVED_KINDS));
    }
    let keys_only = !query.projection.is_empty()
        && query
            .projection
            .iter()
            .all(|p| p.property.as_ref().is_some_and(|p| p.name == "__key__"));
    for a in list {
        if let Some(property) = aggregated(a.operator.as_ref())
            && property != "__key__"
            && !query.projection.is_empty()
        {
            if keys_only {
                return Err(invalid(
                    "Aggregation over non-key properties is not supported for base query that only returns keys.",
                ));
            }
            if !query
                .projection
                .iter()
                .any(|p| p.property.as_ref().is_some_and(|p| p.name == property))
            {
                return Err(invalid(format!(
                    "The aggregation property {property} is not present in the base query's projection."
                )));
            }
        }
    }
    let mut aliases: Vec<String> = Vec::new();
    let mut unnamed = 0;
    for a in list {
        if let Some(property) = aggregated(a.operator.as_ref())
            && reserved(property)
        {
            return Err(invalid(format!(
                "Aggregations are not supported for the property: {property}"
            )));
        }
        let alias = if a.alias.is_empty() {
            unnamed += 1;
            format!("property_{unnamed}")
        } else {
            a.alias.clone()
        };
        if aliases.contains(&alias) {
            return Err(invalid(format!(
                "Aggregation aliases contain duplicate alias: {alias}."
            )));
        }
        aliases.push(alias);
    }
    if query.offset < 0 {
        return Err(invalid("offset is negative"));
    }
    if query.limit.is_some_and(|n| n < 0) {
        return Err(invalid("limit is negative"));
    }
    // The ancestor's key is read here, before its namespace is compared.
    for (filter, _) in properties(all) {
        if operator(filter) == Some(Operator::HasAncestor)
            && let Some(operand) = &filter.value
        {
            check_aggregation_operand_late(name(filter), operand)?;
        }
    }
    ancestor_namespace(all, namespace)?;
    if query.kind.iter().any(|k| k.name.is_empty()) {
        return Err(invalid("The query kind is the empty string."));
    }
    // Without a kind, a sum or an average orders by its property too.
    if query.kind.is_empty()
        && (ordered_beyond_key(query, all)
            || list
                .iter()
                .any(|a| aggregated(a.operator.as_ref()).is_some()))
    {
        return Err(invalid(KINDLESS_ORDER));
    }
    if query
        .projection
        .iter()
        .any(|p| p.property.as_ref().is_none_or(|p| p.name.is_empty()))
    {
        return Err(invalid(EMPTY_SEGMENT));
    }
    projection(query)?;
    for filter in &normalized_filters(query) {
        if name(filter).is_empty() {
            return Err(invalid(EMPTY_SEGMENT));
        }
        if operator(filter) == Some(Operator::HasAncestor) {
            continue;
        }
        if name(filter).len() > MAX_NAME {
            return Err(invalid(format!(
                "property path is longer than {MAX_NAME} bytes."
            )));
        }
        if name(filter) == "__key__" {
            key_operands(filter, namespace)?;
        }
        projected_equality(query, filter)?;
        grouped_equality(query, filter)?;
        if let Some(operand) = &filter.value {
            check_aggregation_operand_late(name(filter), operand)?;
        }
        // An array in an operand, or an element unindexed, is an internal error on the official
        // emulator.
        let unindexed = match (operator(filter), &filter.value) {
            (Some(Operator::NotIn), Some(v)) => match &v.value_type {
                Some(ValueType::ArrayValue(array)) => {
                    array.values.iter().any(|e| e.exclude_from_indexes)
                }
                _ => false,
            },
            (_, Some(v)) => v.exclude_from_indexes,
            _ => false,
        };
        if unindexed
            || operands(filter)
                .iter()
                .any(|v| matches!(v, ValueType::ArrayValue(_)))
        {
            return Err(Status::unknown(""));
        }
        if name(filter) != "__key__" && query.kind.is_empty() {
            return Err(invalid(format!(
                "kind is required for filter: {}",
                name(filter)
            )));
        }
    }
    if names(query).any(|n| n.is_none_or(str::is_empty)) {
        return Err(invalid(EMPTY_SEGMENT));
    }
    grouped_first(query)?;
    cursors_match(query)
}
