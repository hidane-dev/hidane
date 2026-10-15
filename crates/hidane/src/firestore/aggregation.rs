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
//!
//! An enterprise database aggregates otherwise (`tests/fixtures/aggregations_enterprise.json`,
//! #338): see [`Unchecked::check_enterprise`] and [`Aggregations::compute_enterprise`].

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

impl Unchecked {
    /// The checks of the list on an enterprise database (#338): not empty, and the aliases;
    /// no limit on their number, any alias, any field (`__name__` holds no number).
    pub fn check_enterprise(self) -> Result<Aggregations, Status> {
        let aggregations = self.0;
        if aggregations.is_empty() {
            return Err(Status::invalid_argument("Aggregations can not be empty."));
        }
        let mut parsed: Vec<(String, Kind)> = Vec::with_capacity(aggregations.len());
        let mut unnamed = 0;
        for (alias, read) in aggregations {
            let kind = match read {
                Read::Count { up_to } => Kind::Count { up_to },
                Read::Sum(path) => Kind::Sum(path.field_path()),
                Read::Avg(path) => Kind::Avg(path.field_path()),
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

impl Aggregations {
    /// The result of every aggregation over `documents` on an enterprise database (#338), as
    /// the query projects them (`projection`): `count` counts them all (a `count` with `up_to`
    /// is an `UNKNOWN` error without a message); `sum` adds the numbers in turn, as 64-bit
    /// integers while it has seen integers alone, an overflow being an `INVALID_ARGUMENT` "Long
    /// overflow", then as decimals ([`decimal_sum`]); `avg` divides that sum by how many
    /// numbers it found; with no number, both are null.
    pub fn compute_enterprise(
        &self,
        documents: &[Arc<StoredDocument>],
        projection: Option<&[FieldPath]>,
    ) -> Result<Fields, Status> {
        let fields: Vec<_> = documents
            .iter()
            .map(|doc| {
                let fields = doc.fields();
                match projection {
                    Some(paths) => hidane_core::field_path::project(&fields, paths),
                    None => fields,
                }
            })
            .collect();
        let null = || Value {
            value_type: Some(ValueType::NullValue(0)),
        };
        self.0
            .iter()
            .map(|(alias, kind)| {
                let value = match kind {
                    Kind::Count { up_to: Some(_) } => return Err(Status::unknown("")),
                    Kind::Count { up_to: None } => {
                        integer(i128::try_from(documents.len()).unwrap_or(i128::MAX))
                    }
                    Kind::Sum(path) | Kind::Avg(path) => {
                        let numbers: Vec<&ValueType> = fields
                            .iter()
                            .filter_map(|f| get(f, path)?.value_type.as_ref())
                            .filter(|v| {
                                matches!(v, ValueType::IntegerValue(_) | ValueType::DoubleValue(_))
                            })
                            .collect();
                        match enterprise_sum(&numbers)? {
                            None => null(),
                            Some(sum) if matches!(kind, Kind::Sum(_)) => Value {
                                value_type: Some(sum),
                            },
                            #[allow(clippy::cast_precision_loss)]
                            Some(sum) => {
                                let total = match sum {
                                    ValueType::IntegerValue(i) => i as f64,
                                    ValueType::DoubleValue(d) => d,
                                    _ => unreachable!("a sum is a number"),
                                };
                                double(total / numbers.len() as f64)
                            }
                        }
                    }
                };
                Ok((alias.clone(), value))
            })
            .collect()
    }
}

/// The sum of `numbers` (integers and doubles, in the order the query read them) on an
/// enterprise database, `None` without any: 64-bit integers until the first double, whatever
/// comes after it; then every number as a double, added as decimals. Which partial sums
/// overflow depends on that order, which the official emulator does not define.
#[allow(clippy::cast_precision_loss)]
fn enterprise_sum(numbers: &[&ValueType]) -> Result<Option<ValueType>, Status> {
    let mut integers: i64 = 0;
    let mut doubles: Option<Vec<f64>> = None;
    for v in numbers {
        match (v, &mut doubles) {
            (ValueType::IntegerValue(i), None) => {
                integers = integers
                    .checked_add(*i)
                    .ok_or_else(|| Status::invalid_argument("Long overflow"))?;
            }
            (ValueType::IntegerValue(i), Some(doubles)) => doubles.push(*i as f64),
            (ValueType::DoubleValue(d), Some(doubles)) => doubles.push(*d),
            (ValueType::DoubleValue(d), None) => doubles = Some(vec![integers as f64, *d]),
            _ => unreachable!("numbers only"),
        }
    }
    Ok(match doubles {
        _ if numbers.is_empty() => None,
        Some(doubles) => Some(ValueType::DoubleValue(decimal_sum(&doubles))),
        None => Some(ValueType::IntegerValue(integers)),
    })
}

/// The sum of `values` as an enterprise database adds doubles: each as the shortest decimal
/// that reads back as it (Java's `BigDecimal.valueOf`), added exactly, then the double nearest
/// the total (`0.1 + 0.2` is `0.3`). NaN, or infinities of both signs, make NaN; an infinity
/// makes itself.
fn decimal_sum(values: &[f64]) -> f64 {
    if values.iter().any(|v| v.is_nan())
        || (values.contains(&f64::INFINITY) && values.contains(&f64::NEG_INFINITY))
    {
        return f64::NAN;
    }
    if let Some(&infinite) = values.iter().find(|v| v.is_infinite()) {
        return infinite;
    }
    // Each value as digits × 10^power.
    let terms: Vec<(bool, String, i32)> = values
        .iter()
        .filter(|v| **v != 0.0)
        .map(|v| {
            let (digits, exponent) = crate::rest::shortest(v.abs());
            let power = exponent - i32::try_from(digits.len()).unwrap_or(0) + 1;
            (*v < 0.0, digits, power)
        })
        .collect();
    let Some(lowest) = terms.iter().map(|(_, _, power)| *power).min() else {
        return 0.0;
    };
    let mut positive = Natural::default();
    let mut negative = Natural::default();
    for (negative_term, digits, power) in &terms {
        let zeros = usize::try_from(power - lowest).unwrap_or(0);
        let term = Natural::parse(&format!("{digits}{}", "0".repeat(zeros)));
        if *negative_term {
            negative.add(&term);
        } else {
            positive.add(&term);
        }
    }
    let (sign, magnitude) = if positive >= negative {
        ("", positive.minus(&negative))
    } else {
        ("-", negative.minus(&positive))
    };
    format!("{sign}{}e{lowest}", magnitude.decimal())
        .parse()
        .unwrap_or(f64::NAN)
}

/// A natural number in base 10^9, least significant limb first, without leading zero limbs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Natural(Vec<u32>);

const LIMB: u32 = 1_000_000_000;

impl Natural {
    fn parse(digits: &str) -> Self {
        let bytes = digits.as_bytes();
        let mut limbs = Vec::with_capacity(bytes.len() / 9 + 1);
        let mut end = bytes.len();
        while end > 0 {
            let start = end.saturating_sub(9);
            let chunk = std::str::from_utf8(&bytes[start..end]).expect("digits");
            limbs.push(chunk.parse().expect("digits"));
            end = start;
        }
        let mut n = Self(limbs);
        n.trim();
        n
    }

    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn add(&mut self, other: &Self) {
        let mut carry = 0;
        for i in 0..self.0.len().max(other.0.len()) {
            if i == self.0.len() {
                self.0.push(0);
            }
            let sum = self.0[i] + other.0.get(i).copied().unwrap_or(0) + carry;
            self.0[i] = sum % LIMB;
            carry = sum / LIMB;
        }
        if carry > 0 {
            self.0.push(carry);
        }
    }

    /// `self - other`, `other` being at most `self`.
    fn minus(&self, other: &Self) -> Self {
        let mut out = Vec::with_capacity(self.0.len());
        let mut borrow = 0;
        for (i, &limb) in self.0.iter().enumerate() {
            let subtrahend = other.0.get(i).copied().unwrap_or(0) + borrow;
            if limb >= subtrahend {
                out.push(limb - subtrahend);
                borrow = 0;
            } else {
                out.push(limb + LIMB - subtrahend);
                borrow = 1;
            }
        }
        let mut n = Self(out);
        n.trim();
        n
    }

    fn decimal(&self) -> String {
        let Some((last, rest)) = self.0.split_last() else {
            return "0".to_owned();
        };
        let mut text = last.to_string();
        for limb in rest.iter().rev() {
            text.push_str(&format!("{limb:09}"));
        }
        text
    }
}

impl PartialOrd for Natural {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Natural {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enterprise_sums_add_doubles_as_decimals() {
        // Recorded in tests/fixtures/aggregations_enterprise.json.
        assert_eq!(decimal_sum(&[0.1, 0.2]), 0.3);
        assert_eq!(decimal_sum(&[0.1, 0.2, 0.4]), 0.7);
        assert_eq!(
            decimal_sum(&[9_007_199_254_740_992.0, 1.0]),
            9_007_199_254_740_992.0
        );
        assert_eq!(decimal_sum(&[1e308, 1e308]), f64::INFINITY);
        assert!(decimal_sum(&[f64::INFINITY, f64::NEG_INFINITY]).is_nan());
        assert!(decimal_sum(&[1.0, f64::NAN]).is_nan());
        assert_eq!(decimal_sum(&[-0.0]).to_bits(), 0.0_f64.to_bits());
        assert_eq!(decimal_sum(&[1.5, -2.25, 1e-20]), -0.75);
        assert_eq!(decimal_sum(&[1e300, 1e-300, -1e300]), 1e-300);
    }

    #[test]
    fn enterprise_integer_sums_overflow_until_a_double() {
        let int = ValueType::IntegerValue;
        let max = int(i64::MAX);
        let one = int(1);
        let half = ValueType::DoubleValue(0.5);
        assert!(enterprise_sum(&[&max, &one]).is_err());
        assert_eq!(enterprise_sum(&[&one, &one]).unwrap(), Some(int(2)));
        assert_eq!(enterprise_sum(&[]).unwrap(), None);
        assert_eq!(
            enterprise_sum(&[&half, &max, &one]).unwrap(),
            Some(ValueType::DoubleValue(9.223_372_036_854_776e18))
        );
    }
}
