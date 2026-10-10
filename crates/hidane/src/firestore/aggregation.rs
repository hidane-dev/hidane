//! RunAggregationQuery's `count`, `sum` and `avg`, following the official emulator
//! (`tests/fixtures/aggregations.json`, recorded by `tools/oracle/aggregations.py`):
//!
//! - `count` counts the query's documents (after `offset` and `limit`), at most `up_to`.
//! - `sum` adds the integers exactly and stays an integer when only integers were found and
//!   the total fits in 64 bits, even if a partial sum did not; any double makes it a double.
//!   Values that are not numbers are skipped; nothing to add gives the integer 0.
//! - `avg` is always a double, or null when no number was found.
//! - Documents without a field that a `sum` or `avg` reads are left out of the whole query,
//!   `count` included, before `offset` and `limit`.

use std::{collections::BTreeMap, sync::Arc};

use hidane_core::{
    field_path::{FieldPath, get},
    store::StoredDocument,
};
use hidane_proto::google::firestore::v1::{
    Value,
    structured_aggregation_query::{Aggregation, aggregation::Operator},
    structured_query::FieldReference,
    value::ValueType,
};
use tonic::Status;

const MAX_AGGREGATIONS: usize = 5;

#[derive(Debug, Clone)]
enum Kind {
    Count { up_to: Option<u64> },
    Sum(FieldPath),
    Avg(FieldPath),
}

#[derive(Debug, Clone)]
pub struct Aggregations(Vec<(String, Kind)>);

impl Aggregations {
    /// Validates `aggregations`, with the official messages, and names the unnamed ones
    /// `field_1`, `field_2`, … in order.
    pub fn parse(aggregations: &[Aggregation]) -> Result<Self, Status> {
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
        let mut parsed = Vec::with_capacity(aggregations.len());
        let mut unnamed = 0;
        for aggregation in aggregations {
            let kind = match &aggregation.operator {
                None => {
                    return Err(Status::invalid_argument(
                        "Operator field in Aggregation is not set.",
                    ));
                }
                Some(Operator::Count(count)) => Kind::Count {
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
                Some(Operator::Sum(sum)) => Kind::Sum(field(sum.field.as_ref())?),
                Some(Operator::Avg(avg)) => Kind::Avg(field(avg.field.as_ref())?),
            };
            let alias = if aggregation.alias.is_empty() {
                unnamed += 1;
                format!("field_{unnamed}")
            } else {
                aggregation.alias.clone()
            };
            parsed.push((alias, kind));
        }
        for (i, (alias, _)) in parsed.iter().enumerate() {
            if alias.len() >= 4 && alias.starts_with("__") && alias.ends_with("__") {
                return Err(Status::invalid_argument(format!(
                    "The property.name \"{alias}\" is reserved."
                )));
            }
            if parsed[..i].iter().any(|(other, _)| other == alias) {
                return Err(Status::invalid_argument(format!(
                    "Aggregation aliases contain duplicate alias: {alias}."
                )));
            }
        }
        Ok(Self(parsed))
    }

    /// The fields `sum` and `avg` read: documents without them do not take part at all.
    pub fn fields(&self) -> impl Iterator<Item = FieldPath> + '_ {
        self.0.iter().filter_map(|(_, kind)| match kind {
            Kind::Sum(path) | Kind::Avg(path) => Some(path.clone()),
            Kind::Count { .. } => None,
        })
    }

    /// The result of every aggregation over `documents`, by alias.
    pub fn compute(&self, documents: &[Arc<StoredDocument>]) -> BTreeMap<String, Value> {
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

fn field(reference: Option<&FieldReference>) -> Result<FieldPath, Status> {
    let path = reference.map_or("", |r| r.field_path.as_str());
    match path {
        "" => Err(Status::invalid_argument(
            "Invalid empty property path string.",
        )),
        "__name__" => Err(Status::invalid_argument(
            "Aggregations are not supported for the property: __key__",
        )),
        _ => FieldPath::parse(path).map_err(Status::invalid_argument),
    }
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
