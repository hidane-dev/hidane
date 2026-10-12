//! Deciding a list: rules are not filters, so a query is allowed only if its filter makes every
//! document it may return satisfy the conditions. As the official emulator decides it
//! (`tools/oracle/rules_list.py`):
//!
//! - The filter is taken apart into the ways a document may satisfy it: `or`s, and each value
//!   of an `in` or an `array-contains-any`, give one way each. Every way must be allowed, by
//!   the `allow list` statements of the matches covering the collection's path and an unknown
//!   document ID; the first way denied gives the denial.
//! - In each way, `resource` is `{data: …}` holding only the fields the filter names. A field
//!   with one filter, `==` (or `in`, `is null`, `is nan`), holds that value; any other holds a
//!   `constraint` (`super::constraint`). `resource.data.x` for another field is undefined, and
//!   a wildcard that took the document ID is unbound. Ordering, cursors and limits constrain
//!   nothing.
//! - A way with `__name__ ==` (or one name of a `__name__ in`) is decided on that document as
//!   stored (null if there is none), whatever else the filter says, with its path as
//!   `request.path`; other comparisons with the name constrain nothing.

use std::rc::Rc;

use indexmap::IndexMap;

use super::{
    Budget, Context, Decision, Documents, Filter, FilterOp, Method, Request, Trace, Value,
    constraint::{Comparator, Simple},
    denied, no_match, request_value, statements,
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
    if applying.is_empty() {
        return Err(None);
    }
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

/// The ways a document may satisfy `filter`, or `None` if there are too many.
fn ways(filter: Option<&Filter>) -> Option<Vec<Way>> {
    let conjunctions = match filter {
        Some(filter) => dnf(filter)?,
        None => vec![Vec::new()],
    };
    let mut ways: Vec<Way> = conjunctions.into_iter().map(way).collect();
    if ways.is_empty() {
        ways.push(Way::Fields(Vec::new()));
    }
    Some(ways)
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

/// What a field's filters tell of it: its value, or a constraint.
fn known(filters: &[(FilterOp, Value)]) -> Val {
    if let [(op, value)] = filters {
        match op {
            FilterOp::Eq => return Val::from(value),
            FilterOp::IsNull => return Val::Null,
            FilterOp::IsNan => return Val::Float(f64::NAN),
            _ => {}
        }
    }
    let mut simple = Vec::with_capacity(filters.len());
    for (op, value) in filters {
        let one = |comparator, value: &Value| Simple {
            comparator,
            value: Val::from(value),
        };
        match op {
            FilterOp::Eq => simple.push(one(Comparator::Eq, value)),
            FilterOp::Ne => simple.push(one(Comparator::Neq, value)),
            FilterOp::Lt => simple.push(one(Comparator::Lt, value)),
            FilterOp::Le => simple.push(one(Comparator::Lte, value)),
            FilterOp::Gt => simple.push(one(Comparator::Gt, value)),
            FilterOp::Ge => simple.push(one(Comparator::Gte, value)),
            FilterOp::ArrayContains => simple.push(one(Comparator::ListContains, value)),
            FilterOp::NotIn => {
                if let Value::List(values) = value {
                    simple.extend(values.iter().map(|v| one(Comparator::Neq, v)));
                }
            }
            FilterOp::IsNull => simple.push(one(Comparator::Eq, &Value::Null)),
            FilterOp::IsNotNull => simple.push(one(Comparator::Neq, &Value::Null)),
            FilterOp::IsNan => simple.push(one(Comparator::Eq, &Value::Float(f64::NAN))),
            FilterOp::IsNotNan => simple.push(one(Comparator::Neq, &Value::Float(f64::NAN))),
            // Taken apart into ways.
            FilterOp::In | FilterOp::ArrayContainsAny => {}
        }
    }
    Val::Constraint(Rc::from(simple))
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
