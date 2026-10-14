//! Deciding a list: rules are not filters, so a query is allowed only if its filter makes every
//! document it may return satisfy the conditions. As the official emulator decides it
//! (`tools/oracle/rules_list.py`):
//!
//! - The filter is taken apart into the ways a document may satisfy it: `or`s, and each value
//!   of an `in` or an `array-contains-any`, give one way each. A way with the same filters as
//!   one before it (in any order, `n == null` and `n > NaN` alike) is left out. Every way must
//!   be allowed, by the `allow list` statements of the matches covering the collection's path
//!   and an unknown document ID; the first way denied gives the denial.
//! - In each way, `resource` is `{data: …}` holding only the fields the filter names. Each
//!   filter of a field gives comparators and values (`simples`, which shows those of null and
//!   NaN), each kept once. A field left with one `EQ` (`==`, `in`, `is null`, `is nan`, or
//!   `== 1` twice) holds that value, and any other a `constraint` of them
//!   (`super::constraint`). `resource.data.x` for another field is undefined, and a wildcard
//!   that took the document ID is unbound. Ordering, cursors and limits constrain nothing.
//! - A way with `__name__ ==` (or one name of a `__name__ in`) is decided on that document as
//!   stored (null if there is none), whatever else the filter says, with its path as
//!   `request.path`; other comparisons with the name constrain nothing.
//! - Each way counts the expressions it evaluates from zero (`tools/oracle/rules_expressions.py`).

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use indexmap::IndexMap;

use super::{
    Budget, Context, Decision, Documents, Filter, FilterOp, Method, Request, Trace, Value,
    constraint::{Comparator, Simple},
    denied, no_match, request_value, statements,
    trace::hash_val,
    value::{self, Val},
};
use crate::{
    Ruleset, ast,
    paths::{Part, covering_parts},
};

/// More ways than any valid query has (Firestore allows 30 disjunctions).
const MAX_WAYS: usize = 1_000;

/// The fields a way names, each with its filters in order.
type Fields = Vec<(Vec<String>, Vec<(FilterOp, Value)>)>;

/// One way a document may satisfy a filter.
enum Way {
    /// `__name__ ==` this document.
    Document(Vec<String>),
    /// Each field the filter names, with its filters in order.
    Fields(Fields),
}

/// A filter of one way.
enum Atom {
    Field(Vec<String>, FilterOp, Value),
    Name(Vec<String>),
}

pub(super) fn authorize(
    ruleset: &Ruleset,
    service: &ast::Service,
    request: &Request,
    documents: &dyn Documents,
    budget: &mut Budget,
    trace: &mut Trace,
) -> Decision {
    let query = request.query.clone().unwrap_or_default();
    let Some(ways) = ways(query.filter.as_ref()) else {
        return Decision::Deny("\nThe query's filter has too many disjunctions.".to_owned());
    };
    for way in ways {
        // Each way evaluates its expressions from zero.
        budget.expressions = 0;
        let outcome = match way {
            Way::Document(path) => {
                let parts: Vec<Part<'_>> = path.iter().map(|s| Part::Segment(s)).collect();
                let resource = documents
                    .get(&path)
                    .map_or(Val::Null, |d| value::resource(&d));
                decide(
                    ruleset,
                    service,
                    request,
                    Some(&path),
                    &parts,
                    resource,
                    documents,
                    budget,
                    trace,
                )
            }
            Way::Fields(fields) => {
                let mut parts: Vec<Part<'_>> =
                    request.path.iter().map(|s| Part::Segment(s)).collect();
                if let Some(id) = &query.group {
                    parts.push(Part::UnknownRun);
                    if !id.is_empty() {
                        parts.push(Part::Segment(id));
                    }
                }
                parts.push(Part::UnknownId);
                let mut resource = IndexMap::new();
                resource.insert("data".to_owned(), data(&fields));
                let resource = Val::Map(Rc::new(resource));
                decide(
                    ruleset, service, request, None, &parts, resource, documents, budget, trace,
                )
            }
        };
        // The first way denied decides, as on the official emulator.
        match outcome {
            Ok(()) => {}
            Err(None) => return no_match(),
            Err(Some(entries)) => return denied(&entries),
        }
    }
    Decision::Allow
}

/// One way, decided by the statements covering `parts`: `Err(None)` when none does. A way
/// decided on one `document` shows its path as `request.path`.
#[allow(clippy::too_many_arguments)]
fn decide(
    ruleset: &Ruleset,
    service: &ast::Service,
    request: &Request,
    document: Option<&Vec<String>>,
    parts: &[Part<'_>],
    resource: Val,
    documents: &dyn Documents,
    budget: &mut Budget,
    trace: &mut Trace,
) -> Result<(), Option<Vec<String>>> {
    let coverings = covering_parts(ruleset.version, service, parts);
    let applying = statements(&coverings, Method::List);
    let mut request_value = request_value(request, Method::List);
    // A collection group's path is not known (`tools/oracle/rules_list.py`).
    let path = match document {
        Some(path) => Val::path(path.clone()),
        None if parts.contains(&Part::UnknownRun) => Val::Unknown,
        None => Val::path(request.path.clone()),
    };
    if let Val::Map(map) = &mut request_value {
        Rc::make_mut(map).insert("path".to_owned(), path);
    }
    if applying.is_empty() {
        trace.evaluating(Method::List, &request_value, &resource);
        return Err(None);
    }
    let context = Context {
        ruleset,
        service,
        request: request_value,
        resource,
        target: document.unwrap_or(&request.path),
        documents,
    };
    context
        .decide(&applying, Method::List, budget, trace)
        .map_err(Some)
}

/// The ways a document may satisfy `filter`, each once, or `None` if there are too many.
fn ways(filter: Option<&Filter>) -> Option<Vec<Way>> {
    let conjunctions = match filter {
        Some(filter) => dnf(filter)?,
        None => vec![Vec::new()],
    };
    let mut ways: Vec<Way> = Vec::with_capacity(conjunctions.len());
    let mut seen: Vec<Vec<Filtered>> = Vec::with_capacity(conjunctions.len());
    for atoms in conjunctions {
        if let Some(filtered) = signature(&atoms) {
            if seen.iter().any(|other| same_signature(other, &filtered)) {
                continue;
            }
            seen.push(filtered);
        }
        ways.push(way(atoms));
    }
    if ways.is_empty() {
        ways.push(Way::Fields(Vec::new()));
    }
    Some(ways)
}

/// A field and the constraint filters one filter of a way makes, with a hash of both.
type Filtered = (u64, Vec<String>, Vec<Simple>);

/// A way of fields as the official emulator tells ways apart: each of its filters as the
/// constraint filters it makes (`== null` and `> NaN` are one), in any order. `None` for a way
/// with a name.
fn signature(atoms: &[Atom]) -> Option<Vec<Filtered>> {
    let mut filtered = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let Atom::Field(field, op, value) = atom else {
            return None;
        };
        let simple = simples(*op, value);
        let mut hasher = DefaultHasher::new();
        field.hash(&mut hasher);
        for s in &simple {
            s.comparator.hash(&mut hasher);
            hash_val(&s.value, &mut hasher);
        }
        filtered.push((hasher.finish(), field.clone(), simple));
    }
    filtered.sort_unstable_by_key(|(hash, ..)| *hash);
    Some(filtered)
}

/// Whether two signatures have the same filters. Filters whose hashes collide may be sorted
/// apart, which only keeps a way that repeats.
fn same_signature(a: &[Filtered], b: &[Filtered]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((h, f, s), (i, g, t))| {
            h == i && f == g && s.len() == t.len() && s.iter().zip(t).all(|(x, y)| x.same(y))
        })
}

fn way(atoms: Vec<Atom>) -> Way {
    let mut fields: Fields = Vec::new();
    for atom in atoms {
        match atom {
            Atom::Name(path) => return Way::Document(path),
            Atom::Field(field, op, value) => match fields.iter_mut().find(|(f, _)| *f == field) {
                Some((_, filters)) => filters.push((op, value)),
                None => fields.push((field, vec![(op, value)])),
            },
        }
    }
    Way::Fields(fields)
}

/// `filter` in disjunctive normal form, with `in` and `array-contains-any` taken apart.
fn dnf(filter: &Filter) -> Option<Vec<Vec<Atom>>> {
    let out = match filter {
        Filter::And(filters) => {
            let mut product: Vec<Vec<Atom>> = vec![Vec::new()];
            for filter in filters {
                let terms = dnf(filter)?;
                if product.len().saturating_mul(terms.len()) > MAX_WAYS {
                    return None;
                }
                product = product
                    .iter()
                    .flat_map(|left| {
                        terms.iter().map(move |right| {
                            left.iter().chain(right).map(Atom::copy).collect::<Vec<_>>()
                        })
                    })
                    .collect();
            }
            product
        }
        Filter::Or(filters) => {
            let mut sum = Vec::new();
            for filter in filters {
                sum.extend(dnf(filter)?);
                if sum.len() > MAX_WAYS {
                    return None;
                }
            }
            sum
        }
        Filter::Field { field, op, value } => match (op, value) {
            (FilterOp::In, Value::List(values)) => values
                .iter()
                .map(|v| vec![Atom::Field(field.clone(), FilterOp::Eq, v.clone())])
                .collect(),
            (FilterOp::ArrayContainsAny, Value::List(values)) => values
                .iter()
                .map(|v| {
                    vec![Atom::Field(
                        field.clone(),
                        FilterOp::ArrayContains,
                        v.clone(),
                    )]
                })
                .collect(),
            _ => vec![vec![Atom::Field(field.clone(), *op, value.clone())]],
        },
        Filter::Name { op, value } => match (op, value) {
            (FilterOp::Eq, Value::Path(path)) => vec![vec![Atom::Name(path.clone())]],
            (FilterOp::In, Value::List(names)) => names
                .iter()
                .filter_map(|name| match name {
                    Value::Path(path) => Some(vec![Atom::Name(path.clone())]),
                    _ => None,
                })
                .collect(),
            _ => vec![Vec::new()],
        },
    };
    (out.len() <= MAX_WAYS).then_some(out)
}

impl Atom {
    fn copy(&self) -> Self {
        match self {
            Self::Field(field, op, value) => Self::Field(field.clone(), *op, value.clone()),
            Self::Name(path) => Self::Name(path.clone()),
        }
    }
}

/// `resource.data` for one way: the fields the filter names, nested by their segments.
fn data(fields: &Fields) -> Val {
    let mut root = IndexMap::new();
    for (field, filters) in fields {
        insert(&mut root, field, known(filters));
    }
    freeze(root)
}

/// What a field's filters tell of it: its value when they are one `EQ`, or a constraint of
/// them, each once.
fn known(filters: &[(FilterOp, Value)]) -> Val {
    let mut simple: Vec<Simple> = Vec::with_capacity(filters.len());
    for (op, value) in filters {
        for s in simples(*op, value) {
            if !simple.iter().any(|seen| seen.same(&s)) {
                simple.push(s);
            }
        }
    }
    if let [
        Simple {
            comparator: Comparator::Eq,
            value,
        },
    ] = simple.as_slice()
    {
        return value.clone();
    }
    Val::Constraint(Rc::from(simple))
}

/// The filters of a constraint one filter of the query makes. A comparison with null or NaN
/// matches nothing (`n > null`, `n == NaN`, `n in [null]`), and is `EQ true, EQ false`, which
/// no value satisfies; `!= NaN` matches every value but null, and is `NEQ null`; `not-in` is
/// each of its values' `!=`, and `array-contains` null or NaN is `LIST_CONTAINS []`. The unary
/// filters are `EQ null`, `NEQ null`, `EQ NaN` and `NEQ NaN`.
fn simples(op: FilterOp, value: &Value) -> Vec<Simple> {
    let one = |comparator, value| Simple { comparator, value };
    let nan = matches!(value, Value::Float(f) if f.is_nan());
    let null_or_nan = nan || *value == Value::Null;
    let comparator = match op {
        FilterOp::IsNull => return vec![one(Comparator::Eq, Val::Null)],
        FilterOp::IsNotNull => return vec![one(Comparator::Neq, Val::Null)],
        FilterOp::IsNan => return vec![one(Comparator::Eq, Val::Float(f64::NAN))],
        FilterOp::IsNotNan => return vec![one(Comparator::Neq, Val::Float(f64::NAN))],
        FilterOp::NotIn => {
            let Value::List(values) = value else {
                return Vec::new();
            };
            return values
                .iter()
                .flat_map(|v| simples(FilterOp::Ne, v))
                .collect();
        }
        FilterOp::Ne if nan => return vec![one(Comparator::Neq, Val::Null)],
        FilterOp::ArrayContains if null_or_nan => {
            return vec![one(Comparator::ListContains, Val::list(Vec::new()))];
        }
        _ if null_or_nan => {
            return vec![
                one(Comparator::Eq, Val::Bool(true)),
                one(Comparator::Eq, Val::Bool(false)),
            ];
        }
        FilterOp::Eq => Comparator::Eq,
        FilterOp::Ne => Comparator::Neq,
        FilterOp::Lt => Comparator::Lt,
        FilterOp::Le => Comparator::Lte,
        FilterOp::Gt => Comparator::Gt,
        FilterOp::Ge => Comparator::Gte,
        FilterOp::ArrayContains => Comparator::ListContains,
        // Taken apart into ways.
        FilterOp::In | FilterOp::ArrayContainsAny => return Vec::new(),
    };
    vec![one(comparator, Val::from(value))]
}

enum Node {
    Value(Val),
    Map(IndexMap<String, Node>),
}

fn insert(map: &mut IndexMap<String, Node>, field: &[String], value: Val) {
    let Some((first, rest)) = field.split_first() else {
        return;
    };
    if rest.is_empty() {
        map.insert(first.clone(), Node::Value(value));
        return;
    }
    let node = map
        .entry(first.clone())
        .or_insert_with(|| Node::Map(IndexMap::new()));
    if let Node::Value(_) = node {
        *node = Node::Map(IndexMap::new());
    }
    if let Node::Map(inner) = node {
        insert(inner, rest, value);
    }
}

fn freeze(map: IndexMap<String, Node>) -> Val {
    Val::Map(Rc::new(
        map.into_iter()
            .map(|(key, node)| {
                let value = match node {
                    Node::Value(v) => v,
                    Node::Map(inner) => freeze(inner),
                };
                (key, value)
            })
            .collect(),
    ))
}
