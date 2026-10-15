//! Timestamps: `timestamp_add`, `timestamp_subtract`, `timestamp_diff`, `timestamp_extract`,
//! `timestamp_trunc`, `timestamp_to_unix_seconds`, `timestamp_to_unix_millis`,
//! `timestamp_to_unix_micros`, `unix_seconds_to_timestamp`, `unix_millis_to_timestamp`,
//! `unix_micros_to_timestamp`, `current_timestamp`.
//!
//! Timestamps count microseconds, as Firestore keeps them, from 0001-01-01T00:00:00Z to
//! 9999-12-31T23:59:59.999999Z; a result outside that range is an error. The units of
//! `timestamp_add`, `timestamp_subtract` and `timestamp_diff` are fixed lengths up to a day, and
//! `timestamp_diff` counts whole units, toward zero. The parts of `timestamp_extract` and the
//! granularities of `timestamp_trunc` are BigQuery's, on the calendar of UTC or of the IANA time
//! zone of a literal third argument (`Asia/Tokyo`; an offset such as `+05:30` is not one):
//! `dayofweek` counts from Sunday = 1; `week` numbers the weeks that start on a Sunday
//! (`week(monday)` on a Monday, ...) from 0 for the days before the year's first such day;
//! `isoweek` and `isoyear` are ISO 8601's. `current_timestamp` is the time the pipeline reads at.

use std::str::FromStr;

use chrono::{
    DateTime, Datelike, Days, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, NaiveTime,
    Offset, TimeDelta, TimeZone, Timelike, Weekday,
};
use chrono_tz::Tz;
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use prost_types::Timestamp;
use tonic::Status;

use super::{Def, Signature, args, found, math::integer, one_of, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Expr, Scope},
    value,
};

/// 0001-01-01T00:00:00Z and 9999-12-31T23:59:59.999999Z, in microseconds since the epoch.
const MIN_MICROS: i128 = -62_135_596_800_000_000;
const MAX_MICROS: i128 = 253_402_300_799_999_999;

/// The units of `timestamp_add`, `timestamp_subtract` and `timestamp_diff`.
const UNITS: &[&str] = &[
    "microsecond",
    "millisecond",
    "second",
    "minute",
    "hour",
    "day",
];

/// The parts `timestamp_extract` takes.
const PARTS: &[&str] = &[
    "microsecond",
    "millisecond",
    "second",
    "minute",
    "hour",
    "dayofweek",
    "day",
    "dayofyear",
    "week",
    "week(monday)",
    "week(tuesday)",
    "week(wednesday)",
    "week(thursday)",
    "week(friday)",
    "week(saturday)",
    "week(sunday)",
    "isoweek",
    "month",
    "quarter",
    "year",
    "isoyear",
];

/// The granularities `timestamp_trunc` takes.
const GRANULARITIES: &[&str] = &[
    "microsecond",
    "millisecond",
    "second",
    "minute",
    "hour",
    "day",
    "week",
    "week(monday)",
    "week(tuesday)",
    "week(wednesday)",
    "week(thursday)",
    "week(friday)",
    "week(saturday)",
    "week(sunday)",
    "isoweek",
    "month",
    "quarter",
    "year",
    "isoyear",
];

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "timestamp_add",
        signatures: &[Signature::fixed(
            "timestamp_add[T: timestamp | request_timestamp](timestamp: T, unit: \
             (microsecond|millisecond|second|minute|hour|day), amount: int64): timestamp",
            3,
        )],
        check: Some(|args| one_of("timestamp_add", "unit", &args[1], UNITS)),
        eval: |call, scope| shift(call, scope, "timestamp_add", 1),
    },
    Def {
        name: "timestamp_subtract",
        signatures: &[Signature::fixed(
            "timestamp_subtract[T: timestamp | request_timestamp](timestamp: T, unit: \
             (microsecond|millisecond|second|minute|hour|day), amount: int64): timestamp",
            3,
        )],
        check: Some(|args| one_of("timestamp_subtract", "unit", &args[1], UNITS)),
        eval: |call, scope| shift(call, scope, "timestamp_sub", -1),
    },
    Def {
        name: "timestamp_diff",
        signatures: &[Signature::fixed(
            "timestamp_diff[T1: timestamp | request_timestamp, T2: timestamp | \
             request_timestamp](end: T1, start: T2, unit: \
             (microsecond|millisecond|second|minute|hour|day)): int64",
            3,
        )],
        check: Some(|args| one_of("timestamp_diff", "unit", &args[2], UNITS)),
        eval: |call, scope| {
            let values = args(call, scope)?;
            let end = timestamp("timestamp_diff", "Timestamp", values[0].as_ref())?;
            let start = timestamp("timestamp_diff", "Timestamp", values[1].as_ref())?;
            let (Some(end), Some(start)) = (end, start) else {
                return super::null();
            };
            let units = (end - start) / unit_micros(&call.args[2]);
            i64::try_from(units)
                .map(|n| Some(value::int(n)))
                .map_err(|_| invalid("timestamp_diff", "result is out of range"))
        },
    },
    Def {
        name: "timestamp_extract",
        signatures: &[
            Signature::fixed(
                "timestamp_extract[T: timestamp | request_timestamp](timestamp: T, part: \
                 (microsecond|millisecond|second|minute|hour|dayofweek|day|dayofyear|week|\
                 week(monday)|week(tuesday)|week(wednesday)|week(thursday)|week(friday)|\
                 week(saturday)|week(sunday)|isoweek|month|quarter|year|isoyear)): int64",
                2,
            ),
            Signature::fixed(
                "timestamp_extract[T: timestamp | request_timestamp](timestamp: T, part: \
                 (microsecond|millisecond|second|minute|hour|dayofweek|day|dayofyear|week|\
                 week(monday)|week(tuesday)|week(wednesday)|week(thursday)|week(friday)|\
                 week(saturday)|week(sunday)|isoweek|month|quarter|year|isoyear), time_zone: \
                 string): int64",
                3,
            ),
        ],
        check: Some(|args| {
            one_of("timestamp_extract", "part", &args[1], PARTS)?;
            literal_time_zone("timestamp_extract", args)
        }),
        eval: extract,
    },
    Def {
        name: "timestamp_trunc",
        signatures: &[
            Signature::fixed(
                "timestamp_trunc[T: timestamp | request_timestamp](timestamp: T, granularity: \
                 (microsecond|millisecond|second|minute|hour|day|week|week(monday)|\
                 week(tuesday)|week(wednesday)|week(thursday)|week(friday)|week(saturday)|\
                 week(sunday)|isoweek|month|quarter|year|isoyear)): timestamp",
                2,
            ),
            Signature::fixed(
                "timestamp_trunc[T: timestamp | request_timestamp](timestamp: T, granularity: \
                 (microsecond|millisecond|second|minute|hour|day|week|week(monday)|\
                 week(tuesday)|week(wednesday)|week(thursday)|week(friday)|week(saturday)|\
                 week(sunday)|isoweek|month|quarter|year|isoyear), time_zone: string): \
                 timestamp",
                3,
            ),
        ],
        check: Some(|args| {
            one_of("timestamp_trunc", "granularity", &args[1], GRANULARITIES)?;
            literal_time_zone("timestamp_trunc", args)
        }),
        eval: trunc,
    },
    Def {
        name: "timestamp_to_unix_seconds",
        signatures: &[Signature::fixed(
            "timestamp_to_unix_seconds[T: timestamp | request_timestamp](input: T): int64",
            1,
        )],
        check: None,
        eval: |call, scope| to_unix(call, scope, "timestamp_to_unix_seconds", 1_000_000),
    },
    Def {
        name: "timestamp_to_unix_millis",
        signatures: &[Signature::fixed(
            "timestamp_to_unix_millis[T: timestamp | request_timestamp](input: T): int64",
            1,
        )],
        check: None,
        eval: |call, scope| to_unix(call, scope, "timestamp_to_unix_millis", 1_000),
    },
    Def {
        name: "timestamp_to_unix_micros",
        signatures: &[Signature::fixed(
            "timestamp_to_unix_micros[T: timestamp | request_timestamp](input: T): int64",
            1,
        )],
        check: None,
        eval: |call, scope| to_unix(call, scope, "timestamp_to_unix_micros", 1),
    },
    Def {
        name: "unix_seconds_to_timestamp",
        signatures: &[Signature::fixed(
            "unix_seconds_to_timestamp(input: int64): timestamp",
            1,
        )],
        check: None,
        eval: |call, scope| from_unix(call, scope, "unix_seconds_to_timestamp", 1_000_000),
    },
    Def {
        name: "unix_millis_to_timestamp",
        signatures: &[Signature::fixed(
            "unix_millis_to_timestamp(input: int64): timestamp",
            1,
        )],
        check: None,
        eval: |call, scope| from_unix(call, scope, "unix_millis_to_timestamp", 1_000),
    },
    Def {
        name: "unix_micros_to_timestamp",
        signatures: &[Signature::fixed(
            "unix_micros_to_timestamp(input: int64): timestamp",
            1,
        )],
        check: None,
        eval: |call, scope| from_unix(call, scope, "unix_micros_to_timestamp", 1),
    },
    Def {
        name: "current_timestamp",
        signatures: &[Signature::fixed("current_timestamp(): timestamp", 0)],
        check: None,
        eval: |_, scope| Ok(Some(value::timestamp(scope.env.at.to_timestamp()))),
    },
];

/// A timestamp argument, in microseconds: `None` for null or absent, an error naming
/// `expected` for any other type.
fn timestamp(function: &str, expected: &str, v: Option<&Value>) -> Result<Option<i128>, Status> {
    match v.and_then(|v| v.value_type.as_ref()) {
        None | Some(ValueType::NullValue(_)) => Ok(None),
        Some(ValueType::TimestampValue(ts)) => Ok(Some(
            i128::from(ts.seconds) * 1_000_000 + i128::from(ts.nanos.div_euclid(1_000)),
        )),
        Some(_) => Err(requires(function, expected, v.expect("a value"))),
    }
}

/// The timestamp `micros` after the epoch, if Firestore's range has it.
fn timestamp_at(micros: i128) -> Option<Timestamp> {
    if !(MIN_MICROS..=MAX_MICROS).contains(&micros) {
        return None;
    }
    let seconds = i64::try_from(micros.div_euclid(1_000_000)).ok()?;
    let nanos = i32::try_from(micros.rem_euclid(1_000_000) * 1_000).ok()?;
    Some(Timestamp { seconds, nanos })
}

/// `{function}(...) {problem}`: what goes wrong with arguments of the right types.
fn invalid(function: &str, problem: &str) -> Status {
    Status::invalid_argument(format!("{function}(...) {problem}"))
}

/// The length of a literal unit, in microseconds.
fn unit_micros(unit: &Expr) -> i128 {
    match unit.literal_string() {
        Some("microsecond") => 1,
        Some("millisecond") => 1_000,
        Some("second") => 1_000_000,
        Some("minute") => 60_000_000,
        Some("hour") => 3_600_000_000,
        _ => 86_400_000_000,
    }
}

/// `timestamp_add` (`sign` 1) or `timestamp_subtract` (-1), whose errors call it `function`.
fn shift(call: &Call, scope: &Scope<'_>, function: &str, sign: i128) -> Eval {
    let values = args(call, scope)?;
    let ts = timestamp(
        function,
        "Timestamp or RequestTimestamp",
        values[0].as_ref(),
    )?;
    let amount = integer(function, values[2].as_ref())?;
    let (Some(ts), Some(amount)) = (ts, amount) else {
        return super::null();
    };
    let micros = ts + sign * i128::from(amount) * unit_micros(&call.args[1]);
    timestamp_at(micros)
        .map(|ts| Some(value::timestamp(ts)))
        .ok_or_else(|| invalid(function, "result is out of range"))
}

/// `timestamp_to_unix_*`: whole units of `unit` microseconds since the epoch, rounded down.
fn to_unix(call: &Call, scope: &Scope<'_>, function: &str, unit: i128) -> Eval {
    let v = scope.eval(&call.args[0])?;
    let Some(micros) = timestamp(function, "Timestamp", v.as_ref())? else {
        return super::null();
    };
    i64::try_from(micros.div_euclid(unit))
        .map(|n| Some(value::int(n)))
        .map_err(|_| invalid(function, "result is out of range"))
}

/// `unix_*_to_timestamp`: the timestamp a count of `unit` microseconds after the epoch.
fn from_unix(call: &Call, scope: &Scope<'_>, function: &str, unit: i128) -> Eval {
    let v = scope.eval(&call.args[0])?;
    let Some(n) = integer(function, v.as_ref())? else {
        return super::null();
    };
    timestamp_at(i128::from(n) * unit)
        .map(|ts| Some(value::timestamp(ts)))
        .ok_or_else(|| invalid(function, "argument out of range"))
}

/// The check that a time zone argument, if any, is a literal.
fn literal_time_zone(function: &str, args: &[Expr]) -> Result<(), Status> {
    match args.get(2) {
        None | Some(Expr::Constant(_)) => Ok(()),
        Some(expr) => Err(Status::invalid_argument(format!(
            "The 'time_zone' argument of the '{function}' function must be a literal value, \
             found: {}",
            found(expr)
        ))),
    }
}

/// A time zone argument: `None` for null, an error for another type than a string.
fn zone_name<'a>(function: &str, v: Option<&'a Value>) -> Result<Option<&'a str>, Status> {
    match v.and_then(|v| v.value_type.as_ref()) {
        None | Some(ValueType::NullValue(_)) => Ok(None),
        Some(ValueType::StringValue(s)) => Ok(Some(s)),
        Some(_) => Err(requires(function, "String", v.expect("a value"))),
    }
}

/// The arguments of `timestamp_extract` and `timestamp_trunc`: the timestamp in the time zone
/// (UTC unless given), or `None` when either is null or absent.
fn zoned(call: &Call, scope: &Scope<'_>, function: &str) -> Result<Option<DateTime<Tz>>, Status> {
    let values = args(call, scope)?;
    let ts = timestamp(function, "Timestamp", values[0].as_ref())?;
    let name = match values.get(2) {
        None => Some("UTC"),
        Some(v) => zone_name(function, v.as_ref())?,
    };
    let (Some(ts), Some(name)) = (ts, name) else {
        return Ok(None);
    };
    let zone = Tz::from_str(name).map_err(|_| invalid(function, "given an invalid timezone"))?;
    let utc = i64::try_from(ts)
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| invalid(function, "argument out of range"))?;
    Ok(Some(utc.with_timezone(&zone)))
}

/// The day weeks start on for `week`, `week(monday)`, ..., `isoweek`.
fn first_day(granularity: &str) -> Weekday {
    match granularity {
        "week(monday)" | "isoweek" => Weekday::Mon,
        "week(tuesday)" => Weekday::Tue,
        "week(wednesday)" => Weekday::Wed,
        "week(thursday)" => Weekday::Thu,
        "week(friday)" => Weekday::Fri,
        "week(saturday)" => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

/// The week of `date` in its year, with weeks starting on `first`: 0 before the year's first
/// `first`, as BigQuery's `WEEK(<WEEKDAY>)`.
fn week_number(date: NaiveDate, first: Weekday) -> u32 {
    let new_year = date.with_ordinal(1).expect("every year has a first day");
    let until_first = first.days_since(new_year.weekday());
    (date.ordinal0() + 7 - until_first) / 7
}

fn extract(call: &Call, scope: &Scope<'_>) -> Eval {
    let Some(at) = zoned(call, scope, "timestamp_extract")? else {
        return super::null();
    };
    let time = at.naive_local();
    let date = time.date();
    let part = call.args[1].literal_string().unwrap_or_default();
    let n = match part {
        "microsecond" => i64::from(time.nanosecond() / 1_000),
        "millisecond" => i64::from(time.nanosecond() / 1_000_000),
        "second" => i64::from(time.second()),
        "minute" => i64::from(time.minute()),
        "hour" => i64::from(time.hour()),
        "dayofweek" => i64::from(date.weekday().number_from_sunday()),
        "day" => i64::from(date.day()),
        "dayofyear" => i64::from(date.ordinal()),
        "isoweek" => i64::from(date.iso_week().week()),
        "month" => i64::from(date.month()),
        "quarter" => i64::from(date.month0() / 3 + 1),
        "year" => i64::from(date.year()),
        "isoyear" => i64::from(date.iso_week().year()),
        week => i64::from(week_number(date, first_day(week))),
    };
    Ok(Some(value::int(n)))
}

fn trunc(call: &Call, scope: &Scope<'_>) -> Eval {
    let Some(at) = zoned(call, scope, "timestamp_trunc")? else {
        return super::null();
    };
    let time = at.naive_local();
    let date = time.date();
    let midnight = |date: Option<NaiveDate>| date.map(|d| d.and_time(NaiveTime::MIN));
    let truncated = match call.args[1].literal_string().unwrap_or_default() {
        "microsecond" => Some(time),
        "millisecond" => time.with_nanosecond(time.nanosecond() / 1_000_000 * 1_000_000),
        "second" => time.with_nanosecond(0),
        "minute" => date.and_hms_opt(time.hour(), time.minute(), 0),
        "hour" => date.and_hms_opt(time.hour(), 0, 0),
        "day" => midnight(Some(date)),
        "month" => midnight(date.with_day(1)),
        "quarter" => midnight(NaiveDate::from_ymd_opt(
            date.year(),
            date.month0() / 3 * 3 + 1,
            1,
        )),
        "year" => midnight(date.with_ordinal(1)),
        "isoyear" => midnight(NaiveDate::from_isoywd_opt(
            date.iso_week().year(),
            1,
            Weekday::Mon,
        )),
        week => {
            let back = date.weekday().days_since(first_day(week));
            midnight(date.checked_sub_days(Days::new(u64::from(back))))
        }
    };
    truncated
        .map(|local| instant(local, at.timezone(), at.offset().fix()))
        .and_then(timestamp_at)
        .map(|ts| Some(value::timestamp(ts)))
        .ok_or_else(|| invalid("timestamp_trunc", "result is out of range"))
}

/// The instant of the local time `local` in `zone`, in microseconds, resolved as `java.time`
/// resolves it: a time the zone repeats keeps `offset` if it can, else takes the earlier
/// offset; a time it skips moves later by the length of the gap.
fn instant(local: NaiveDateTime, zone: Tz, offset: FixedOffset) -> i128 {
    let utc = match zone.from_local_datetime(&local) {
        LocalResult::Single(t) => t.naive_utc(),
        LocalResult::Ambiguous(earlier, later) => {
            if later.offset().fix() == offset {
                later.naive_utc()
            } else {
                earlier.naive_utc()
            }
        }
        LocalResult::None => {
            // The offset before the gap: the one a day earlier, no gap being longer.
            let before = zone
                .offset_from_utc_datetime(&(local - TimeDelta::days(1)))
                .fix();
            local - before
        }
    };
    i128::from(utc.and_utc().timestamp_micros())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn weeks_start_on_their_first_day() {
        // 2026-01-01 is a Thursday: Sunday weeks start on the 4th, Thursday weeks on the 1st.
        assert_eq!(week_number(date(2026, 1, 2), Weekday::Sun), 0);
        assert_eq!(week_number(date(2026, 1, 4), Weekday::Sun), 1);
        assert_eq!(week_number(date(2026, 1, 10), Weekday::Sun), 1);
        assert_eq!(week_number(date(2026, 1, 11), Weekday::Sun), 2);
        assert_eq!(week_number(date(2026, 1, 1), Weekday::Thu), 1);
        assert_eq!(week_number(date(2026, 1, 7), Weekday::Thu), 1);
        assert_eq!(week_number(date(2026, 1, 8), Weekday::Thu), 2);
        assert_eq!(week_number(date(2026, 12, 31), Weekday::Sun), 52);
    }

    #[test]
    fn skipped_and_repeated_times_resolve_as_java_time() {
        let zone: Tz = "America/New_York".parse().unwrap();
        let at = |h, m| date(2026, 3, 8).and_hms_opt(h, m, 0).unwrap();
        let edt = FixedOffset::west_opt(4 * 3600).unwrap();
        let est = FixedOffset::west_opt(5 * 3600).unwrap();
        // 02:30 does not exist on 2026-03-08: it is 03:30 EDT, 07:30Z.
        let skipped = DateTime::parse_from_rfc3339("2026-03-08T07:30:00Z").unwrap();
        assert_eq!(
            instant(at(2, 30), zone, est),
            i128::from(skipped.timestamp_micros())
        );
        // 01:30 happens twice on 2026-11-01: EDT (05:30Z) first, EST (06:30Z) next.
        let repeated = date(2026, 11, 1).and_hms_opt(1, 30, 0).unwrap();
        let first = DateTime::parse_from_rfc3339("2026-11-01T05:30:00Z").unwrap();
        let second = DateTime::parse_from_rfc3339("2026-11-01T06:30:00Z").unwrap();
        assert_eq!(
            instant(repeated, zone, edt),
            i128::from(first.timestamp_micros())
        );
        assert_eq!(
            instant(repeated, zone, est),
            i128::from(second.timestamp_micros())
        );
    }

    #[test]
    fn the_range_is_firestore_s() {
        assert!(timestamp_at(MIN_MICROS).is_some());
        assert!(timestamp_at(MIN_MICROS - 1).is_none());
        assert_eq!(
            timestamp_at(MAX_MICROS),
            Some(Timestamp {
                seconds: 253_402_300_799,
                nanos: 999_999_000
            })
        );
        assert!(timestamp_at(MAX_MICROS + 1).is_none());
        assert_eq!(
            timestamp_at(-1),
            Some(Timestamp {
                seconds: -1,
                nanos: 999_999_000
            })
        );
    }
}
