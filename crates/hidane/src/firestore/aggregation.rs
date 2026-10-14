//! RunAggregationQuery's `count`, `sum` and `avg`, following the official emulator
//! (`tests/fixtures/aggregations.json`, recorded by `tools/oracle/aggregations.py`):
//!
//! - `count` counts the query's documents (after `offset` and `limit`), at most `up_to`.
//! - `sum` adds the integers exactly and stays an integer when only integers were found and
//!   the total fits in 64 bits, even if a partial sum did not; any double makes it a double.
//!   Values that are not numbers are skipped; nothing to add gives the integer 0.
//! - `avg` is always a double, or null when no number was found.
//! - Documents without a field that a `sum` or `avg` reads are left out of the whole query,
//!   `count` included, before `offset` and `limit`; those fields order the query, so `offset`
//!   and `limit` take the smallest values first (`Query::require`).

use std::sync::Arc;

use hidane_core::{
    field_path::Fields,
    field_path::{FieldPath, get},
    store::StoredDocument,
};
use hidane_proto::google::firestore::v1::{
    Value,
    structured_aggregation_query::{
        Aggregation,
        aggregation::{Avg, Operator, Sum},
    },
    structured_query::FieldReference,
    value::ValueType,
};
use tonic::Status;

use super::property_path::PropertyPath;

const MAX_AGGREGATIONS: usize = 5;

#[derive(Debug, Clone)]
enum Kind {
    Count { up_to: Option<u64> },
    Sum(FieldPath),
    Avg(FieldPath),
}

#[derive(Debug, Clone)]
pub struct Aggregations(Vec<(String, Kind)>);

/// An aggregation read but not yet checked with the list: see [`Aggregations::read`].
enum Read {
    Count { up_to: Option<u64> },
    Sum(PropertyPath),
    Avg(PropertyPath),
}

/// Aggregations read but not yet checked as a list: see [`Aggregations::read`].
pub struct Unchecked(Vec<(String, Read)>);

impl Aggregations {
    /// The official emulator checks an aggregation query in two passes around its query's
    /// (`tools/oracle/aggregations.py`, #262): this one, before the query, reads each
    /// aggregation (its operator, a `count`'s `up_to`, the path a `sum` or an `avg` reads, as
    /// [`PropertyPath::read`] does); [`Unchecked::check`], after what the query names, checks
    /// the list.
    pub fn read(aggregations: &[Aggregation]) -> Result<Unchecked, Status> {
        let mut read = Vec::with_capacity(aggregations.len());
        for aggregation in aggregations {
            let kind = match &aggregation.operator {
                None => {
                    return Err(Status::invalid_argument(
                        "Operator field in Aggregation is not set.",
                    ));
                }
                Some(Operator::Count(count)) => Read::Count {
                    up_to: match count.up_to {
                        None => None,
                        Some(n) => Some(u64::try_from(n).map_err(|_| {
                            Status::invalid_argument(
                                "The `up_to` value in a COUNT aggregation must be greater than \
                                 or equal to zero.",
                            )
                        })?),
                    },
                },
                Some(Operator::Sum(Sum { field })) => Read::Sum(path(field.as_ref())?),
                Some(Operator::Avg(Avg { field })) => Read::Avg(path(field.as_ref())?),
            };
            read.push((aggregation.alias.clone(), kind));
        }
        Ok(Unchecked(read))
    }
}

impl Unchecked {
    /// The checks of the list (see [`Aggregations::read`]), with the official messages
    /// (`tools/oracle/aggregations.py`, #262): not empty, at most five, no reserved alias, the
    /// query's projection ([`check_projection`]), then each in turn: what a `sum` or an `avg`
    /// reads (not `__name__` or `__type__` itself, [`PropertyPath::check_query`]) and an alias
    /// no other before it has. The unnamed ones are `field_1`, `field_2`, … in order.
    pub fn check(self, projection: Option<&[FieldPath]>) -> Result<Aggregations, Status> {
        let aggregations = self.0;
        if aggregations.is_empty() {
            return Err(Status::invalid_argument("Aggregations can not be empty."));
        }
        if aggregations.len() > MAX_AGGREGATIONS {
            return Err(Status::invalid_argument(format!(
                "The maximum number of aggregations allowed in an aggregation query is \
                 {MAX_AGGREGATIONS}. Received: {}",
                aggregations.len()
            )));
        }
        if let Some((alias, _)) = aggregations
            .iter()
            .find(|(alias, _)| alias.len() >= 4 && alias.starts_with("__") && alias.ends_with("__"))
        {
            return Err(Status::invalid_argument(format!(
                "The property.name \"{alias}\" is reserved."
            )));
        }
        if let Some(projection) = projection {
            check_projection(projection, &aggregations)?;
        }
        let mut parsed: Vec<(String, Kind)> = Vec::with_capacity(aggregations.len());
        let mut unnamed = 0;
        for (alias, read) in aggregations {
            let kind = match read {
                Read::Count { up_to } => Kind::Count { up_to },
                Read::Sum(path) => Kind::Sum(field(&path)?),
                Read::Avg(path) => Kind::Avg(field(&path)?),
            };
            let alias = if alias.is_empty() {
                unnamed += 1;
                format!("field_{unnamed}")
            } else {
                alias
            };
            if parsed.iter().any(|(other, _)| *other == alias) {
                return Err(Status::invalid_argument(format!(
                    "Aggregation aliases contain duplicate alias: {alias}."
                )));
            }
            parsed.push((alias, kind));
        }
        Ok(Aggregations(parsed))
    }
}

/// A `sum` or an `avg` next to a projection (`tools/oracle/aggregations.py`, #262): a
/// projection of no field but the name returns keys only, so a field other than the name is
/// refused; any other projection refuses a field it does not hold, in the aggregations' order;
/// then any `sum` or `avg` at all is refused.
fn check_projection(
    projection: &[FieldPath],
    aggregations: &[(String, Read)],
) -> Result<(), Status> {
    let fields: Vec<&PropertyPath> = aggregations
        .iter()
        .filter_map(|(_, read)| match read {
            Read::Sum(path) | Read::Avg(path) => Some(path),
            Read::Count { .. } => None,
        })
        .collect();
    if fields.is_empty() {
        return Ok(());
    }
    if projection.is_empty() {
        if fields.iter().any(|path| !path.is_name()) {
            return Err(Status::invalid_argument(
                "Aggregation over non-key properties is not supported for base query that only \
                 returns keys.",
            ));
        }
    } else if let Some(missing) = fields.iter().find(|path| {
        let segments = path.field_path();
        !path.is_name()
            && !projection
                .iter()
                .any(|held| segments.segments().starts_with(held.segments()))
    }) {
        return Err(Status::invalid_argument(format!(
            "The aggregation property {missing} is not present in the base query's property \
             mask."
        )));
    }
    Err(Status::invalid_argument(
        "Cannot apply property masks when aggregation fields are present.",
    ))
}

/// The field a `sum` or an `avg` reads, checked with the list.
fn field(path: &PropertyPath) -> Result<FieldPath, Status> {
    if path.is_name() || path.is_type() {
        return Err(Status::invalid_argument(format!(
            "Aggregations are not supported for the property: {path}"
        )));
    }
    path.check_query()?;
    Ok(path.field_path())
}

impl Aggregations {
    /// The fields `sum` and `avg` read: documents without them do not take part at all.
    pub fn fields(&self) -> impl Iterator<Item = FieldPath> + '_ {
        self.0.iter().filter_map(|(_, kind)| match kind {
            Kind::Sum(path) | Kind::Avg(path) => Some(path.clone()),
            Kind::Count { .. } => None,
        })
    }

    /// The result of every aggregation over `documents`, by alias, in the order of the request
    /// (as on the official emulator, #108).
    pub fn compute(&self, documents: &[Arc<StoredDocument>]) -> Fields {
        let fields: Vec<_> = documents.iter().map(|doc| doc.fields()).collect();
        self.0
            .iter()
            .map(|(alias, kind)| {
                let value = match kind {
                    Kind::Count { up_to } => {
                        let count = u64::try_from(documents.len()).unwrap_or(u64::MAX);
                        integer(i128::from(up_to.map_or(count, |n| count.min(n))))
                    }
                    Kind::Sum(path) => {
                        let total = Total::of(fields.iter().filter_map(|f| get(f, path)));
                        if total.doubles {
                            double(total.as_f64())
                        } else {
                            integer(total.integers)
                        }
                    }
                    Kind::Avg(path) => {
                        let total = Total::of(fields.iter().filter_map(|f| get(f, path)));
                        if total.count == 0 {
                            Value {
                                value_type: Some(ValueType::NullValue(0)),
                            }
                        } else {
                            double(total.as_f64() / total.count as f64)
                        }
                    }
                };
                (alias.clone(), value)
            })
            .collect()
    }
}

/// The path a `sum` or an `avg` reads, read as [`PropertyPath::read`] does; the rest is checked
/// with the list.
fn path(reference: Option<&FieldReference>) -> Result<PropertyPath, Status> {
    PropertyPath::read(reference.map_or("", |r| r.field_path.as_str()))
}

/// The numbers of a field: integers exactly, doubles apart.
struct Total {
    integers: i128,
    sum_of_doubles: f64,
    doubles: bool,
    count: u64,
}

impl Total {
    fn of<'a>(values: impl Iterator<Item = &'a Value>) -> Self {
        let mut total = Self {
            integers: 0,
            sum_of_doubles: 0.0,
            doubles: false,
            count: 0,
        };
        for value in values {
            match value.value_type {
                Some(ValueType::IntegerValue(i)) => total.integers += i128::from(i),
                Some(ValueType::DoubleValue(d)) => {
                    total.sum_of_doubles += d;
                    total.doubles = true;
                }
                _ => continue,
            }
            total.count += 1;
        }
        total
    }

    #[allow(clippy::cast_precision_loss)]
    fn as_f64(&self) -> f64 {
        self.integers as f64 + self.sum_of_doubles
    }
}

/// An integer result, or a double when it does not fit in 64 bits.
#[allow(clippy::cast_precision_loss)]
fn integer(n: i128) -> Value {
    Value {
        value_type: Some(match i64::try_from(n) {
            Ok(n) => ValueType::IntegerValue(n),
            Err(_) => ValueType::DoubleValue(n as f64),
        }),
    }
}

fn double(d: f64) -> Value {
    Value {
        value_type: Some(ValueType::DoubleValue(d)),
    }
}
