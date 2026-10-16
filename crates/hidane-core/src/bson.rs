//! The BSON types of an enterprise database (#338), as the SDKs write them: maps of one reserved
//! key. `{__min__: null}` and `{__max__: null}` sort before and after every other value;
//! `{__oid__: "<24 hex digits>"}` is an object ID; `{__regex__: {pattern, options}}` a regular
//! expression; `{__int__: <integer>}` a 32-bit integer and `{__decimal128__: "<decimal>"}` a
//! 128-bit decimal, both numbers; `{__request_timestamp__: {seconds, increment}}` a BSON
//! timestamp; `{__binary__: <subtype byte, then the data>}` bytes with a subtype
//! (`tests/fixtures/bson_types.json`).

use std::cmp::Ordering;

use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};

pub const MIN_KEY: &str = "__min__";
pub const MAX_KEY: &str = "__max__";
pub const OBJECT_ID: &str = "__oid__";
pub const REGEX: &str = "__regex__";
pub const INT32: &str = "__int__";
pub const DECIMAL128: &str = "__decimal128__";
pub const TIMESTAMP: &str = "__request_timestamp__";
pub const BINARY: &str = "__binary__";

/// The keys of the BSON types.
pub const KEYS: &[&str] = &[
    MIN_KEY, MAX_KEY, OBJECT_ID, REGEX, INT32, DECIMAL128, TIMESTAMP, BINARY,
];

/// A BSON value, read from its map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bson<'a> {
    MinKey,
    MaxKey,
    ObjectId(&'a str),
    Regex {
        pattern: &'a str,
        options: &'a str,
    },
    Int32(i64),
    Decimal128(&'a str),
    Timestamp {
        seconds: i64,
        increment: i64,
    },
    /// The subtype, then the data.
    Binary(&'a [u8]),
}

fn field<'a>(map: &'a MapValue, name: &str) -> Option<&'a ValueType> {
    map.fields.get(name).and_then(|v| v.value_type.as_ref())
}

/// The BSON value `map` holds: one of the reserved keys alone, with a value of its type.
pub fn bson(map: &MapValue) -> Option<Bson<'_>> {
    if map.fields.len() != 1 {
        return None;
    }
    let (key, value) = map.fields.iter().next()?;
    let inner = value.value_type.as_ref();
    Some(match (key.as_str(), inner) {
        (MIN_KEY, Some(ValueType::NullValue(_))) => Bson::MinKey,
        (MAX_KEY, Some(ValueType::NullValue(_))) => Bson::MaxKey,
        (OBJECT_ID, Some(ValueType::StringValue(s))) => Bson::ObjectId(s),
        (REGEX, Some(ValueType::MapValue(m))) => match (field(m, "pattern"), field(m, "options")) {
            (Some(ValueType::StringValue(pattern)), Some(ValueType::StringValue(options))) => {
                Bson::Regex { pattern, options }
            }
            _ => return None,
        },
        (INT32, Some(ValueType::IntegerValue(i))) => Bson::Int32(*i),
        (DECIMAL128, Some(ValueType::StringValue(s))) => Bson::Decimal128(s),
        (TIMESTAMP, Some(ValueType::MapValue(m))) => {
            match (field(m, "seconds"), field(m, "increment")) {
                (
                    Some(ValueType::IntegerValue(seconds)),
                    Some(ValueType::IntegerValue(increment)),
                ) => Bson::Timestamp {
                    seconds: *seconds,
                    increment: *increment,
                },
                _ => return None,
            }
        }
        (BINARY, Some(ValueType::BytesValue(b))) => Bson::Binary(b),
        _ => return None,
    })
}

/// The BSON value `value` holds, if it is one.
pub fn of(value: &Value) -> Option<Bson<'_>> {
    match &value.value_type {
        Some(ValueType::MapValue(map)) => bson(map),
        _ => None,
    }
}

/// The BSON values in `fields` as an enterprise database stores them: a decimal in its
/// canonical form ([`Decimal`]'s `Display`), a regular expression's and a BSON timestamp's map
/// without keys of their own.
pub fn normalize(fields: &mut crate::field_path::Fields) {
    for value in fields.values_mut() {
        normalize_value(value);
    }
}

fn normalize_value(value: &mut Value) {
    let Some(ValueType::MapValue(map)) = &mut value.value_type else {
        if let Some(ValueType::ArrayValue(array)) = &mut value.value_type {
            array.values.iter_mut().for_each(normalize_value);
        }
        return;
    };
    let kept: Option<&[&str]> = match bson(map) {
        None => {
            normalize(&mut map.fields);
            return;
        }
        Some(Bson::Decimal128(text)) => {
            if let Some(decimal) = Decimal::parse(text) {
                let canonical = decimal.to_string();
                if let Some(Value {
                    value_type: Some(ValueType::StringValue(s)),
                }) = map.fields.get_mut(DECIMAL128)
                {
                    *s = canonical;
                }
            }
            None
        }
        Some(Bson::Regex { .. }) => Some(&["pattern", "options"]),
        Some(Bson::Timestamp { .. }) => Some(&["seconds", "increment"]),
        Some(_) => None,
    };
    if let Some(kept) = kept
        && let Some(Value {
            value_type: Some(ValueType::MapValue(inner)),
        }) = map.fields.values_mut().next()
    {
        let mut fields = crate::field_path::Fields::new();
        for key in kept {
            if let Some(v) = inner.fields.get(key) {
                fields.insert((*key).to_owned(), v.clone());
            }
        }
        inner.fields = fields;
    }
}

/// A 128-bit decimal (IEEE 754-2008 decimal128), as Java's `org.bson.types.Decimal128` reads
/// and prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decimal {
    NaN,
    Infinity {
        negative: bool,
    },
    /// `coefficient × 10^exponent`, the coefficient at most 34 digits.
    Finite {
        negative: bool,
        coefficient: u128,
        exponent: i32,
    },
}

const MAX_DIGITS: usize = 34;
const MAX_EXPONENT: i64 = 6111;
const MIN_EXPONENT: i64 = -6176;

impl Decimal {
    /// Reads a decimal as `Decimal128.parse` does: `NaN`, `Infinity` (or `Inf`, either signed,
    /// any case), or a decimal (`-1.2e3`), held exactly in 34 digits once trailing zeros are
    /// dropped and the exponent clamped; `None` otherwise ("Invalid decimal128 string").
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "nan" | "-nan" => return Some(Self::NaN),
            "inf" | "+inf" | "infinity" | "+infinity" => {
                return Some(Self::Infinity { negative: false });
            }
            "-inf" | "-infinity" => return Some(Self::Infinity { negative: true }),
            _ => {}
        }
        let (negative, rest) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(at) => (&rest[..at], Some(&rest[at + 1..])),
            None => (rest, None),
        };
        let exponent: i64 = match exponent {
            Some(e) => {
                let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                e.parse::<i32>().ok()?.into()
            }
            None => 0,
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty() && fraction.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let digits: String = format!("{whole}{fraction}");
        let digits = digits.trim_start_matches('0');
        let mut exponent = exponent - i64::try_from(fraction.len()).ok()?;
        let mut digits = digits.to_owned();
        // Round exactly to 34 digits: only trailing zeros may go.
        while digits.len() > MAX_DIGITS {
            if !digits.ends_with('0') {
                return None;
            }
            digits.pop();
            exponent += 1;
        }
        if digits.is_empty() {
            exponent = exponent.clamp(MIN_EXPONENT, MAX_EXPONENT);
        } else if exponent > MAX_EXPONENT {
            let diff = usize::try_from(exponent - MAX_EXPONENT).ok()?;
            if diff + digits.len() > MAX_DIGITS {
                return None;
            }
            digits.push_str(&"0".repeat(diff));
            exponent = MAX_EXPONENT;
        } else if exponent < MIN_EXPONENT {
            let diff = usize::try_from(MIN_EXPONENT - exponent).ok()?;
            if diff > digits.len() || !digits[digits.len() - diff..].bytes().all(|b| b == b'0') {
                return None;
            }
            digits.truncate(digits.len() - diff);
            exponent = MIN_EXPONENT;
        }
        Some(Self::Finite {
            negative,
            coefficient: if digits.is_empty() {
                0
            } else {
                digits.parse().ok()?
            },
            exponent: i32::try_from(exponent).ok()?,
        })
    }

    /// The decimal as an exact number, or `None` for NaN and the infinities.
    fn exact(&self) -> Option<Exact> {
        match *self {
            Self::Finite {
                negative,
                coefficient,
                exponent,
            } => Some(Exact::new(
                negative,
                &coefficient.to_string(),
                i64::from(exponent),
            )),
            _ => None,
        }
    }
}

impl std::fmt::Display for Decimal {
    /// `Decimal128.toString()`: plain notation for an exponent at most 0 and an adjusted
    /// exponent at least -6, else scientific (`-1.2E+3`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (negative, coefficient, exponent) = match *self {
            Self::NaN => return f.write_str("NaN"),
            Self::Infinity { negative } => {
                return f.write_str(if negative { "-Infinity" } else { "Infinity" });
            }
            Self::Finite {
                negative,
                coefficient,
                exponent,
            } => (negative, coefficient, i64::from(exponent)),
        };
        let significand = coefficient.to_string();
        let length = i64::try_from(significand.len()).unwrap_or(0);
        let adjusted = exponent + length - 1;
        if negative {
            f.write_str("-")?;
        }
        if exponent <= 0 && adjusted >= -6 {
            if exponent == 0 {
                return f.write_str(&significand);
            }
            let pad = -exponent - length;
            if pad >= 0 {
                let zeros = "0".repeat(usize::try_from(pad).unwrap_or(0));
                write!(f, "0.{zeros}{significand}")
            } else {
                let point = usize::try_from(-pad).unwrap_or(0);
                write!(f, "{}.{}", &significand[..point], &significand[point..])
            }
        } else {
            f.write_str(&significand[..1])?;
            if significand.len() > 1 {
                write!(f, ".{}", &significand[1..])?;
            }
            write!(f, "E{}{adjusted}", if adjusted > 0 { "+" } else { "" })
        }
    }
}

/// A finite number exactly: a sign, its significant digits (no leading or trailing zero; none
/// for zero), and the power of ten of the place after the first digit (`0.d1d2… × 10^point`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Exact {
    negative: bool,
    digits: Vec<u8>,
    point: i64,
}

impl Exact {
    /// `digits × 10^exponent`.
    fn new(negative: bool, digits: &str, exponent: i64) -> Self {
        let trimmed = digits.trim_start_matches('0');
        let without_trailing = trimmed.trim_end_matches('0');
        let point = exponent + i64::try_from(trimmed.len()).unwrap_or(0);
        Self {
            negative: negative && !without_trailing.is_empty(),
            digits: without_trailing.bytes().collect(),
            point,
        }
    }

    fn of_double(d: f64) -> Self {
        // Rust prints a double's exact decimal value when asked for enough digits.
        let text = format!("{:.1100e}", d.abs());
        let (mantissa, exponent) = text.split_once('e').expect("an exponent");
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        let exponent: i64 = exponent.parse().expect("an exponent");
        Self::new(
            d < 0.0,
            &digits,
            exponent - i64::try_from(digits.len()).unwrap_or(0) + 1,
        )
    }

    fn cmp(&self, other: &Self) -> Ordering {
        match (self.digits.is_empty(), other.digits.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {}
        }
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitudes = self
            .point
            .cmp(&other.point)
            .then_with(|| self.digits.cmp(&other.digits));
        if self.negative {
            magnitudes.reverse()
        } else {
            magnitudes
        }
    }
}

/// Whether `value` is NaN: a double, or a decimal128 that reads `NaN`.
pub fn is_nan(value: &Value) -> bool {
    match &value.value_type {
        Some(ValueType::DoubleValue(d)) => d.is_nan(),
        Some(ValueType::MapValue(map)) => matches!(
            bson(map),
            Some(Bson::Decimal128(text)) if matches!(Decimal::parse(text), Some(Decimal::NaN))
        ),
        _ => false,
    }
}

/// A number of any kind, for comparisons.
#[derive(Debug, Clone, Copy)]
pub enum AnyNumber {
    Integer(i64),
    Double(f64),
    Decimal(Decimal),
}

impl AnyNumber {
    /// The number `value` holds: an integer, a double, a 32-bit integer or a decimal128.
    pub fn of(value: &Value) -> Option<Self> {
        match &value.value_type {
            Some(ValueType::IntegerValue(i)) => Some(Self::Integer(*i)),
            Some(ValueType::DoubleValue(d)) => Some(Self::Double(*d)),
            Some(ValueType::MapValue(map)) => match bson(map)? {
                Bson::Int32(i) => Some(Self::Integer(i)),
                Bson::Decimal128(text) => Decimal::parse(text).map(Self::Decimal),
                _ => None,
            },
            _ => None,
        }
    }

    fn is_nan(self) -> bool {
        matches!(self, Self::Double(d) if d.is_nan()) || matches!(self, Self::Decimal(Decimal::NaN))
    }

    /// `+1` or `-1` for an infinity.
    fn infinity(self) -> Option<i8> {
        match self {
            Self::Double(d) if d.is_infinite() => Some(if d > 0.0 { 1 } else { -1 }),
            Self::Decimal(Decimal::Infinity { negative }) => Some(if negative { -1 } else { 1 }),
            _ => None,
        }
    }

    fn exact(self) -> Option<Exact> {
        match self {
            Self::Integer(i) => Some(Exact::new(i < 0, &i.unsigned_abs().to_string(), 0)),
            Self::Double(d) if d.is_finite() => Some(Exact::of_double(d)),
            Self::Decimal(d) => d.exact(),
            Self::Double(_) => None,
        }
    }
}

/// Firestore's order of numbers of any kind: NaN first (every NaN equal), then by exact value,
/// `-0 == 0`.
pub fn compare_numbers(a: AnyNumber, b: AnyNumber) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }
    match (a.infinity(), b.infinity()) {
        (Some(x), Some(y)) => return x.cmp(&y),
        (Some(x), None) => {
            return if x > 0 {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        (None, Some(y)) => {
            return if y > 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        (None, None) => {}
    }
    match (a, b) {
        (AnyNumber::Integer(x), AnyNumber::Integer(y)) => x.cmp(&y),
        (AnyNumber::Double(x), AnyNumber::Double(y)) => {
            x.partial_cmp(&y).unwrap_or(Ordering::Equal)
        }
        _ => match (a.exact(), b.exact()) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => Ordering::Equal,
        },
    }
}

/// Bytes with a subtype, plain bytes being subtype 0: by subtype, then the data.
pub fn compare_bytes(a: (u8, &[u8]), b: (u8, &[u8])) -> Ordering {
    a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_print_as_java_does() {
        for (text, printed) in [
            ("-1.2e3", "-1.2E+3"),
            ("4.2e+3", "4.2E+3"),
            ("-1.2e-3", "-0.0012"),
            ("1.2e-3", "0.0012"),
            ("-1", "-1"),
            ("-0", "-0"),
            ("0", "0"),
            ("-0.0", "-0.0"),
            ("0.0", "0.0"),
            ("1", "1"),
            ("NaN", "NaN"),
            ("-Infinity", "-Infinity"),
            ("Infinity", "Infinity"),
            (
                "0.1234567890123456789012345678901234",
                "0.1234567890123456789012345678901234",
            ),
            (
                "1234567890123456789012345678901234",
                "1234567890123456789012345678901234",
            ),
            ("1e-7", "1E-7"),
            ("0.000001", "0.000001"),
            ("123e-2", "1.23"),
            ("0e3", "0E+3"),
        ] {
            assert_eq!(Decimal::parse(text).unwrap().to_string(), printed, "{text}");
        }
    }

    #[test]
    fn invalid_decimals() {
        for text in [
            "",
            "1 23. 4",
            "abc",
            "1e6145",
            "0.12345678901234567890123456789012345",
            ".",
            "1e",
            "--1",
        ] {
            assert_eq!(Decimal::parse(text), None, "{text}");
        }
    }

    #[test]
    fn numbers_compare_exactly() {
        let dec = |s: &str| AnyNumber::Decimal(Decimal::parse(s).unwrap());
        assert_eq!(
            compare_numbers(dec("1"), AnyNumber::Integer(1)),
            Ordering::Equal
        );
        assert_eq!(
            compare_numbers(dec("1.0"), AnyNumber::Double(1.0)),
            Ordering::Equal
        );
        assert_eq!(
            compare_numbers(dec("0.1"), AnyNumber::Double(0.1)),
            Ordering::Less
        );
        assert_eq!(
            compare_numbers(dec("-0"), AnyNumber::Integer(0)),
            Ordering::Equal
        );
        assert_eq!(
            compare_numbers(dec("9223372036854775808"), AnyNumber::Integer(i64::MAX)),
            Ordering::Greater
        );
        assert_eq!(
            compare_numbers(dec("NaN"), AnyNumber::Double(f64::NAN)),
            Ordering::Equal
        );
        assert_eq!(
            compare_numbers(dec("-Infinity"), AnyNumber::Double(f64::NEG_INFINITY)),
            Ordering::Equal
        );
        assert_eq!(
            compare_numbers(dec("-1200"), AnyNumber::Integer(-5)),
            Ordering::Less
        );
        assert_eq!(
            compare_numbers(dec("0.5"), AnyNumber::Double(0.5)),
            Ordering::Equal
        );
    }
}
