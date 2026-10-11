//! The built-in functions and methods, with the official messages for wrong arguments.

use std::rc::Rc;

use indexmap::IndexMap;

use super::{
    interp::{Interp, string_size},
    time::{civil_from_days, days_from_civil, days_in_month, parse_rfc3339},
    value::{
        Error, MapDiff, NANOS, R, Val, contains, java_double, loose_eq, parse_java_double,
        parse_java_long, strict_eq, to_set, utf16_cmp, utf16_slice,
    },
};

/// The mean radius of the Earth in meters, as `distance()` uses it (measured).
const EARTH_RADIUS: f64 = 6_371_009.0;

fn types(args: &[Val]) -> String {
    args.iter()
        .map(Val::type_name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The official errors for a call of `name` that matches none of `signatures` (each the
/// parameter types of one overload): a wrong number of arguments, or wrong types.
fn mismatch(name: &str, signatures: &[&[&str]], args: &[Val]) -> Error {
    let expected: Vec<String> = signatures
        .iter()
        .map(|params| format!("{name}({})", params.join(", ")))
        .collect();
    if signatures.iter().all(|params| params.len() != args.len()) {
        Error::arity(args.len(), &expected)
    } else {
        Error::unsupported(&format!("{name}({})", types(args)), &expected)
    }
}

/// Checks the number of arguments of a function with one form.
fn arity(name: &str, signature: &[&str], args: &[Val]) -> Result<(), Error> {
    if args.len() == signature.len() {
        Ok(())
    } else {
        Err(mismatch(name, &[signature], args))
    }
}

/// `string()`: ints, floats (as Java prints them), null, strings, bools and paths.
pub(crate) fn to_string(v: &Val) -> Result<String, Error> {
    Ok(match v {
        Val::Int(i) => i.to_string(),
        Val::Float(f) => java_double(*f),
        Val::Null => "null".to_owned(),
        Val::Str(s) => s.to_string(),
        Val::Bool(b) => b.to_string(),
        Val::Path(p) => path_string(p),
        other => {
            return Err(Error::unsupported(
                &format!("string({})", other.type_name()),
                &["int", "float", "null", "string", "bool", "path"].map(|t| format!("string({t})")),
            ));
        }
    })
}

fn path_string(segments: &[String]) -> String {
    if segments.is_empty() {
        return "/".to_owned();
    }
    segments.iter().map(|s| format!("/{s}")).collect()
}

/// A global function: `int()`, `get()`, …
pub(crate) fn global(interp: &mut Interp<'_>, name: &str, args: Vec<Val>) -> R {
    let one = |args: &[Val], types: &[&[&str]]| -> Result<(), Error> {
        if args.len() == 1 {
            Ok(())
        } else {
            Err(mismatch(name, types, args))
        }
    };
    match name {
        "int" => {
            let sig: &[&[&str]] = &[&["int"], &["float"], &["string"]];
            one(&args, sig)?;
            match &args[0] {
                Val::Int(i) => Ok(Val::Int(*i)),
                // Java's cast: toward zero, saturating, NaN to 0.
                Val::Float(f) => Ok(Val::Int(*f as i64)),
                Val::Str(s) => parse_java_long(s)
                    .map(Val::Int)
                    .ok_or_else(|| Error::conversion(s, "string", "int")),
                _ => Err(mismatch(name, sig, &args)),
            }
        }
        "float" => {
            let sig: &[&[&str]] = &[&["int"], &["float"], &["string"]];
            one(&args, sig)?;
            match &args[0] {
                Val::Int(i) => Ok(Val::Float(*i as f64)),
                Val::Float(f) => Ok(Val::Float(*f)),
                Val::Str(s) => parse_java_double(s)
                    .map(Val::Float)
                    .ok_or_else(|| Error::conversion(s, "string", "float")),
                _ => Err(mismatch(name, sig, &args)),
            }
        }
        "string" => {
            one(
                &args,
                &[
                    &["int"],
                    &["float"],
                    &["null"],
                    &["string"],
                    &["bool"],
                    &["path"],
                ],
            )?;
            to_string(&args[0]).map(Val::string)
        }
        "path" => {
            arity(name, &["string"], &args)?;
            match &args[0] {
                Val::Str(s) => parse_path(s),
                _ => Err(mismatch(name, &[&["string"]], &args)),
            }
        }
        "debug" => {
            arity(name, &["any"], &args)?;
            Ok(args.into_iter().next().unwrap_or(Val::Null))
        }
        "get" | "exists" | "getAfter" | "existsAfter" | "value" => {
            arity(name, &["path"], &args)?;
            let Val::Path(path) = &args[0] else {
                return Err(mismatch(name, &[&["path"]], &args));
            };
            let after = name.ends_with("After");
            let found = interp.lookup(name, path, after)?;
            Ok(if name.starts_with("exists") {
                Val::Bool(found.is_some())
            } else {
                found.unwrap_or(Val::Null)
            })
        }
        _ => Err(Error::not_found(name)),
    }
}

/// `path('a/b')`: with or without a leading slash, and no empty segment.
fn parse_path(s: &str) -> R {
    if s.is_empty() {
        return Err(Error("Empty Path Template is not allowed.".to_owned()));
    }
    let body = s.strip_prefix('/').unwrap_or(s);
    let segments: Vec<String> = body.split('/').map(str::to_owned).collect();
    if segments.iter().any(String::is_empty) {
        return Err(Error(format!(
            "Invalid Path Template {s}. Empty segment is not allowed."
        )));
    }
    Ok(Val::path(segments))
}

fn number(v: &Val) -> Option<f64> {
    match v {
        Val::Int(i) => Some(*i as f64),
        Val::Float(f) => Some(*f),
        _ => None,
    }
}

fn int_args(name: &str, sig: &[&str], args: &[Val]) -> Result<Vec<i64>, Error> {
    arity(name, sig, args)?;
    args.iter()
        .zip(sig)
        .map(|(a, t)| match (a, *t) {
            (Val::Int(i), "int") => Ok(*i),
            _ => Err(mismatch(name, &[sig], args)),
        })
        .collect()
}

/// `namespace.function(…)`.
pub(crate) fn namespaced(name: &str, args: Vec<Val>) -> R {
    match name {
        "math.abs" => {
            let sig: &[&[&str]] = &[&["int"], &["float"]];
            if args.len() != 1 {
                return Err(mismatch(name, sig, &args));
            }
            match &args[0] {
                Val::Int(i) => Ok(Val::Int(i.wrapping_abs())),
                Val::Float(f) => Ok(Val::Float(f.abs())),
                _ => Err(mismatch(name, sig, &args)),
            }
        }
        "math.ceil" | "math.floor" | "math.round" | "math.sqrt" | "math.isNaN" => {
            arity(name, &["float"], &args)?;
            let Some(x) = number(&args[0]) else {
                return Err(mismatch(name, &[&["float"]], &args));
            };
            Ok(match name {
                "math.ceil" => Val::Float(x.ceil()),
                "math.floor" => Val::Float(x.floor()),
                "math.sqrt" => Val::Float(x.sqrt()),
                "math.isNaN" => Val::Bool(x.is_nan()),
                _ => match args[0] {
                    Val::Int(i) => Val::Int(i),
                    _ => Val::Int(java_round(x)),
                },
            })
        }
        "math.pow" => {
            arity(name, &["float", "float"], &args)?;
            match (number(&args[0]), number(&args[1])) {
                (Some(x), Some(y)) => Ok(Val::Float(x.powf(y))),
                _ => Err(mismatch(name, &[&["float", "float"]], &args)),
            }
        }
        "duration.abs" => {
            arity(name, &["duration"], &args)?;
            match args[0] {
                Val::Duration(d) => Val::duration(d.abs()),
                _ => Err(mismatch(name, &[&["duration"]], &args)),
            }
        }
        "duration.time" => {
            let p = int_args(name, &["int", "int", "int", "int"], &args)?;
            let [h, m, s, n] = [p[0], p[1], p[2], p[3]].map(i128::from);
            Val::duration(((h * 60 + m) * 60 + s) * NANOS + n)
        }
        "duration.value" => {
            arity(name, &["int", "string"], &args)?;
            let (Val::Int(n), Val::Str(unit)) = (&args[0], &args[1]) else {
                return Err(mismatch(name, &[&["int", "string"]], &args));
            };
            let scale: i128 = match &**unit {
                "w" => 7 * 86_400 * NANOS,
                "d" => 86_400 * NANOS,
                "h" => 3_600 * NANOS,
                "m" => 60 * NANOS,
                "s" => NANOS,
                "ms" => 1_000_000,
                "ns" => 1,
                // The official emulator reports an unknown unit this way.
                other => return Err(Error::regex(other)),
            };
            Val::duration(i128::from(*n) * scale)
        }
        "timestamp.date" => {
            let p = int_args(name, &["int", "int", "int"], &args)?;
            let (year, month, day) = (p[0], p[1], p[2]);
            if !(1..=9999).contains(&year) {
                return Err(Error(format!(
                    "Timestamp should be between 0001-01-01T00.00.00Z and \
                     9999-12-31T23.59.59Z. year: {year}."
                )));
            }
            if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
                return Err(Error(format!(
                    "Date is not valid. year: {year}, month: {month} day: {day}."
                )));
            }
            Val::timestamp(days_from_civil(year, month, day) * 86_400, 0)
        }
        "timestamp.value" => {
            let sig: &[&[&str]] = &[&["int"], &["string"]];
            if args.len() != 1 {
                return Err(mismatch(name, sig, &args));
            }
            match &args[0] {
                Val::Int(ms) => Val::timestamp_nanos(i128::from(*ms) * 1_000_000),
                Val::Str(s) => match parse_rfc3339(s) {
                    Some((seconds, nanos)) => {
                        Val::timestamp_nanos(seconds * NANOS + i128::from(nanos))
                    }
                    None => Err(Error::timestamp()),
                },
                _ => Err(mismatch(name, sig, &args)),
            }
        }
        "latlng.value" => {
            arity(name, &["float", "float"], &args)?;
            let (Some(lat), Some(lng)) = (number(&args[0]), number(&args[1])) else {
                return Err(mismatch(name, &[&["float", "float"]], &args));
            };
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
                return Err(Error(format!(
                    "Latitude should be in [-90,90] range and longitude should be in [-180,180] \
                     range. latitude: {}, longitude: {}.",
                    java_double(lat),
                    java_double(lng)
                )));
            }
            Ok(Val::LatLng(lat, lng))
        }
        "hashing.crc32" | "hashing.crc32c" | "hashing.md5" | "hashing.sha256" => {
            let sig: &[&[&str]] = &[&["bytes"], &["string"]];
            if args.len() != 1 {
                return Err(mismatch(name, sig, &args));
            }
            let data: &[u8] = match &args[0] {
                Val::Bytes(b) => b,
                Val::Str(s) => s.as_bytes(),
                _ => return Err(mismatch(name, sig, &args)),
            };
            Ok(Val::Bytes(Rc::from(hash(name, data))))
        }
        _ => Err(Error::not_found(name)),
    }
}

/// Java's `Math.round`: the nearest int, halves up, saturating, NaN to 0.
fn java_round(x: f64) -> i64 {
    let floor = x.floor();
    let rounded = if x - floor >= 0.5 { floor + 1.0 } else { floor };
    rounded as i64
}

fn hash(name: &str, data: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    match name {
        "hashing.crc32" => crc32(0xEDB8_8320, data).to_le_bytes().to_vec(),
        "hashing.crc32c" => crc32(0x82F6_3B78, data).to_le_bytes().to_vec(),
        "hashing.md5" => md5::Md5::digest(data).to_vec(),
        _ => sha2::Sha256::digest(data).to_vec(),
    }
}

/// A reflected CRC-32 with `polynomial` (IEEE or Castagnoli).
fn crc32(polynomial: u32, data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ polynomial
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Whether `name` is a global function. A call of one that is not fails before its arguments
/// are evaluated.
pub(crate) fn is_global(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "float"
            | "string"
            | "path"
            | "debug"
            | "get"
            | "exists"
            | "getAfter"
            | "existsAfter"
            | "value"
    )
}

/// Whether `name` (`math.abs`) is a namespaced function.
pub(crate) fn is_namespaced(name: &str) -> bool {
    matches!(
        name,
        "math.abs"
            | "math.ceil"
            | "math.floor"
            | "math.round"
            | "math.sqrt"
            | "math.isNaN"
            | "math.pow"
            | "duration.abs"
            | "duration.time"
            | "duration.value"
            | "timestamp.date"
            | "timestamp.value"
            | "latlng.value"
            | "hashing.crc32"
            | "hashing.crc32c"
            | "hashing.md5"
            | "hashing.sha256"
    )
}

/// Whether a value of `type_name` has the method `name`.
pub(crate) fn has_method(type_name: &str, name: &str) -> bool {
    signatures(type_name, name).is_some()
}

/// The methods each type has, with the parameter types of each form.
fn signatures(type_name: &str, method: &str) -> Option<&'static [&'static [&'static str]]> {
    const NONE: &[&[&str]] = &[&[]];
    Some(match (type_name, method) {
        ("string", "size" | "lower" | "upper" | "trim" | "toUtf8") => NONE,
        ("string", "matches" | "split") => &[&["string"]],
        ("string", "replace") => &[&["string", "string"]],
        ("bytes", "size" | "toBase64" | "toHexString") => NONE,
        ("list", "size" | "toSet") => NONE,
        ("list", "hasAll" | "concat" | "removeAll" | "hasAny" | "hasOnly") => &[&["list"]],
        ("list", "join") => &[&["string"]],
        ("map", "get") => &[&["string", "any"], &["list", "any"]],
        ("map", "size" | "keys" | "values") => NONE,
        ("map", "diff") => &[&["map"]],
        ("set", "size") => NONE,
        ("set", "hasAll" | "hasAny" | "hasOnly") => &[&["set"], &["list"]],
        ("set", "difference" | "intersection" | "union") => &[&["set"]],
        (
            "map_diff",
            "addedKeys" | "affectedKeys" | "changedKeys" | "removedKeys" | "unchangedKeys",
        ) => NONE,
        (
            "timestamp",
            "date" | "time" | "toMillis" | "year" | "month" | "day" | "hours" | "minutes"
            | "seconds" | "nanos" | "dayOfWeek" | "dayOfYear",
        ) => NONE,
        ("duration", "seconds" | "nanos") => NONE,
        ("latlng", "distance") => &[&["latlng"]],
        ("latlng", "latitude" | "longitude") => NONE,
        ("path", "bind") => &[&["map"]],
        ("constraint", "hasAny") => &[&["list"]],
        _ => return None,
    })
}

/// `receiver.method(args)`; the receiver is not null.
pub(crate) fn method(interp: &mut Interp<'_>, receiver: &Val, name: &str, args: Vec<Val>) -> R {
    let type_name = receiver.type_name();
    let Some(signatures) = signatures(type_name, name) else {
        return Err(Error::not_found(name));
    };
    let fits = |params: &&[&str]| {
        params.len() == args.len()
            && params
                .iter()
                .zip(&args)
                .all(|(p, a)| *p == "any" || *p == a.type_name())
    };
    if !signatures.iter().any(fits) {
        if let Val::Constraint(_) = receiver {
            // The official emulator names a list's method here.
            return Err(Error::unsupported(
                &format!("constraint.{name}({})", types(&args)),
                &[format!("list.{name}(list)")],
            ));
        }
        return Err(mismatch(&format!("{type_name}.{name}"), signatures, &args));
    }
    match receiver {
        Val::Str(s) => string_method(interp, s, name, &args),
        Val::Bytes(b) => Ok(match name {
            "size" => Val::Int(b.len() as i64),
            "toBase64" => Val::string(base64_url(b)),
            _ => Val::string(b.iter().map(|x| format!("{x:02X}")).collect()),
        }),
        Val::List(items) => list_method(items, name, &args),
        Val::Map(map) => map_method(map, name, &args),
        Val::Set(items) => set_method(items, name, &args),
        Val::MapDiff(diff) => {
            let keys = match name {
                "addedKeys" => diff.added.clone(),
                "removedKeys" => diff.removed.clone(),
                "changedKeys" => diff.changed.clone(),
                "unchangedKeys" => diff.unchanged.clone(),
                _ => diff
                    .added
                    .iter()
                    .chain(&diff.removed)
                    .chain(&diff.changed)
                    .cloned()
                    .collect(),
            };
            Ok(to_set(keys.into_iter().map(Val::string)))
        }
        Val::Timestamp(seconds, nanos) => Ok(timestamp_method(*seconds, *nanos, name)),
        Val::Duration(d) => Ok(Val::Int(match name {
            "seconds" => (d / NANOS) as i64,
            _ => (d % NANOS) as i64,
        })),
        Val::LatLng(lat, lng) => Ok(match (name, &args[..]) {
            ("latitude", _) => Val::Float(*lat),
            ("longitude", _) => Val::Float(*lng),
            (_, [Val::LatLng(lat2, lng2)]) => Val::Float(distance(*lat, *lng, *lat2, *lng2)),
            _ => unreachable!("the signature check passed"),
        }),
        Val::Path(path) => {
            let Val::Map(bindings) = &args[0] else {
                unreachable!("the signature check passed")
            };
            if bindings.is_empty() {
                Ok(Val::Path(path.clone()))
            } else {
                // Paths built in rules have no variables to bind.
                let names: Vec<&str> = bindings.keys().map(String::as_str).collect();
                Err(Error(format!(
                    "Variable(s) {} do not exist in the path template.",
                    names.join(", ")
                )))
            }
        }
        Val::Constraint(c) => {
            let Val::List(items) = &args[0] else {
                unreachable!("the signature check passed")
            };
            Ok(Val::Bool(super::constraint::has_any(c, items)))
        }
        _ => Err(Error::not_found(name)),
    }
}

/// Great-circle distance in meters (haversine).
fn distance(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lng2 - lng1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS * h.sqrt().asin()
}

fn timestamp_method(seconds: i64, nanos: u32, name: &str) -> Val {
    let (days, secs) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    Val::Int(match name {
        "year" => year,
        "month" => month,
        "day" => day,
        "hours" => secs / 3600,
        "minutes" => secs / 60 % 60,
        "seconds" => secs % 60,
        "nanos" => i64::from(nanos),
        "toMillis" => seconds * 1000 + i64::from(nanos / 1_000_000),
        // Monday is 1.
        "dayOfWeek" => (days + 3).rem_euclid(7) + 1,
        "dayOfYear" => days - days_from_civil(year, 1, 1) + 1,
        "date" => return Val::Timestamp(days * 86_400, 0),
        _ => {
            return Val::Duration(i128::from(secs) * NANOS + i128::from(nanos));
        }
    })
}

fn string_method(interp: &mut Interp<'_>, s: &str, name: &str, args: &[Val]) -> R {
    let arg = |i: usize| match &args[i] {
        Val::Str(a) => a.clone(),
        _ => unreachable!("the signature check passed"),
    };
    Ok(match name {
        "size" => Val::Int(string_size(s)),
        // Only ASCII letters change case.
        "lower" => Val::string(s.to_ascii_lowercase()),
        "upper" => Val::string(s.to_ascii_uppercase()),
        // Java's `trim`: control characters and spaces.
        "trim" => Val::str(s.trim_matches(|c: char| c <= ' ')),
        "toUtf8" => Val::Bytes(Rc::from(s.as_bytes())),
        "matches" => Val::Bool(interp.regex(&arg(0), true)?.is_match(s)),
        "split" => {
            let re = interp.regex(&arg(0), false)?;
            let mut parts: Vec<&str> = re.split(s).collect();
            // Trailing empty strings are dropped, but one is kept for an empty result.
            while parts.len() > 1 && parts.last() == Some(&"") {
                parts.pop();
            }
            Val::list(parts.into_iter().map(Val::str).collect())
        }
        _ => {
            let re = interp.regex(&arg(0), false)?;
            Val::string(replace_all(&re, s, &arg(1))?)
        }
    })
}

/// `replace()`, with Java's replacement syntax: `$1`, `${name}` and `\` escapes.
fn replace_all(re: &regex::Regex, s: &str, replacement: &str) -> Result<String, Error> {
    let invalid = || Error(format!("Invalid replacement string: {replacement}."));
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let m = caps.get(0).expect("group 0 always matches");
        out.push_str(&s[last..m.start()]);
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => out.push(chars.next().ok_or_else(invalid)?),
                '$' => {
                    let group = if chars.peek() == Some(&'{') {
                        chars.next();
                        let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
                        caps.name(&name).ok_or_else(invalid)?.as_str()
                    } else {
                        // The longest group number that exists, as Java reads it.
                        let first = chars
                            .next()
                            .and_then(|c| c.to_digit(10))
                            .ok_or_else(invalid)?;
                        let mut number = first as usize;
                        while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                            let longer = number * 10 + d as usize;
                            if longer >= caps.len() {
                                break;
                            }
                            number = longer;
                            chars.next();
                        }
                        if number >= caps.len() {
                            return Err(invalid());
                        }
                        caps.get(number).map_or("", |g| g.as_str())
                    };
                    out.push_str(group);
                }
                c => out.push(c),
            }
        }
        last = m.end();
    }
    out.push_str(&s[last..]);
    Ok(out)
}

fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize])
            } else {
                '='
            });
        }
    }
    out
}

fn items(v: &Val) -> &[Val] {
    match v {
        Val::List(items) | Val::Set(items) => items,
        _ => &[],
    }
}

fn list_method(list: &Rc<[Val]>, name: &str, args: &[Val]) -> R {
    let other = args.first().map_or(&[][..], items);
    Ok(match name {
        "size" => Val::Int(list.len() as i64),
        "toSet" => to_set(list.iter().cloned()),
        "hasAll" => Val::Bool(other.iter().all(|x| contains(list, x, strict_eq))),
        "hasAny" => Val::Bool(other.iter().any(|x| contains(list, x, strict_eq))),
        "hasOnly" => Val::Bool(list.iter().all(|x| contains(other, x, strict_eq))),
        "concat" => Val::list(list.iter().chain(other).cloned().collect()),
        "removeAll" => Val::list(
            list.iter()
                .filter(|x| !contains(other, x, strict_eq))
                .cloned()
                .collect(),
        ),
        _ => {
            let Val::Str(separator) = &args[0] else {
                unreachable!("the signature check passed")
            };
            let parts = list.iter().map(to_string).collect::<Result<Vec<_>, _>>()?;
            Val::string(parts.join(separator))
        }
    })
}

fn set_method(set: &Rc<[Val]>, name: &str, args: &[Val]) -> R {
    let other = args.first().map_or(&[][..], items);
    Ok(match name {
        "size" => Val::Int(set.len() as i64),
        "hasAll" => Val::Bool(other.iter().all(|x| contains(set, x, loose_eq))),
        "hasAny" => Val::Bool(other.iter().any(|x| contains(set, x, loose_eq))),
        "hasOnly" => Val::Bool(set.iter().all(|x| contains(other, x, loose_eq))),
        "union" => to_set(set.iter().chain(other).cloned()),
        "intersection" => to_set(set.iter().filter(|x| contains(other, x, loose_eq)).cloned()),
        _ => to_set(
            set.iter()
                .filter(|x| !contains(other, x, loose_eq))
                .cloned(),
        ),
    })
}

fn map_method(map: &Rc<IndexMap<String, Val>>, name: &str, args: &[Val]) -> R {
    Ok(match name {
        "size" => Val::Int(map.len() as i64),
        // Keys come sorted; values in the map's order.
        "keys" => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            Val::list(keys.into_iter().map(|k| Val::str(k)).collect())
        }
        "values" => Val::list(map.values().cloned().collect()),
        "get" => match &args[0] {
            Val::Str(key) => map.get(&**key).cloned().unwrap_or_else(|| args[1].clone()),
            _ => {
                let mut current = Val::Map(map.clone());
                for key in items(&args[0]) {
                    let Val::Str(key) = key else {
                        return Err(Error::type_error(key.type_name(), "string"));
                    };
                    let next = match &current {
                        Val::Map(m) => m.get(&**key).cloned(),
                        _ => None,
                    };
                    match next {
                        Some(v) => current = v,
                        None => return Ok(args[1].clone()),
                    }
                }
                current
            }
        },
        _ => {
            let Val::Map(other) = &args[0] else {
                unreachable!("the signature check passed")
            };
            let mut diff = MapDiff {
                added: Vec::new(),
                removed: Vec::new(),
                changed: Vec::new(),
                unchanged: Vec::new(),
            };
            for (k, v) in map.iter() {
                match other.get(k) {
                    None => diff.added.push(k.clone()),
                    Some(w) if loose_eq(v, w) => diff.unchanged.push(k.clone()),
                    Some(_) => diff.changed.push(k.clone()),
                }
            }
            diff.removed = other
                .keys()
                .filter(|k| !map.contains_key(*k))
                .cloned()
                .collect();
            Val::MapDiff(Rc::new(diff))
        }
    })
}

/// `object[index]`.
pub(crate) fn index(object: &Val, index: &Val) -> R {
    let unsupported = |expected: &[&str]| {
        Error::unsupported(
            &format!("{}[{}]", object.type_name(), index.type_name()),
            &expected
                .iter()
                .map(|t| format!("{}[{t}]", object.type_name()))
                .collect::<Vec<_>>(),
        )
    };
    let position = |i: i64, size: usize| {
        usize::try_from(i)
            .ok()
            .filter(|&i| i < size)
            .ok_or_else(|| Error::index(i, size))
    };
    match (object, index) {
        (Val::List(items), Val::Int(i)) => Ok(items[position(*i, items.len())?].clone()),
        (Val::List(_), _) => Err(unsupported(&["int"])),
        (Val::Map(map), Val::Str(key)) => {
            super::value::read(map.get(&**key)).unwrap_or_else(|| Err(Error::property(key)))
        }
        (Val::Map(_), _) => Err(unsupported(&["string"])),
        (Val::Str(s), Val::Int(i)) => {
            let at = position(*i, string_size(s) as usize)?;
            Ok(Val::string(utf16_slice(s, at, at + 1)))
        }
        (Val::Str(_), _) => Err(unsupported(&["int"])),
        (Val::Path(segments), Val::Int(_)) if super::value::has_unknown(segments) => {
            Err(Error::unbound_template())
        }
        (Val::Path(segments), Val::Int(i)) => {
            Ok(Val::str(&segments[position(*i, segments.len())?]))
        }
        (Val::Path(_), Val::Str(key)) => Err(Error::property(key)),
        (Val::Path(_), _) => Err(unsupported(&["string", "int"])),
        (Val::Null, _) => Err(Error::null()),
        _ => Err(Error::not_found("[]")),
    }
}

/// `object[start:end]`: either bound may be null. The start must be an index of the object and
/// so must the one before the end.
pub(crate) fn slice(object: &Val, start: &Val, end: &Val) -> R {
    let size = match object {
        Val::List(items) => items.len(),
        Val::Str(s) => string_size(s) as usize,
        Val::Path(segments) if super::value::has_unknown(segments) => {
            return Err(Error::unbound_template());
        }
        Val::Path(segments) => segments.len(),
        Val::Null => return Err(Error::null()),
        _ => return Err(Error::not_found("@range")),
    };
    let bound = |v: &Val| match v {
        Val::Int(i) => Ok(Some(*i)),
        Val::Null => Ok(None),
        _ => Err(()),
    };
    let (Ok(from), Ok(to)) = (bound(start), bound(end)) else {
        let t = object.type_name();
        return Err(Error::unsupported(
            &format!("{t}[{}:{}]", start.type_name(), end.type_name()),
            &[
                ("null", "null"),
                ("null", "int"),
                ("int", "null"),
                ("int", "int"),
            ]
            .map(|(a, b)| format!("{t}[{a}:{b}]")),
        ));
    };
    let inside = |i: i128| i >= 0 && i < size as i128;
    if let Some(i) = from
        && !inside(i128::from(i))
    {
        return Err(Error::index(i, size));
    }
    if let Some(j) = to
        && !inside(i128::from(j) - 1)
    {
        return Err(Error::index(j.saturating_sub(1), size));
    }
    let (from, to) = (from.unwrap_or(0), to.unwrap_or(size as i64));
    if from > to {
        return Err(Error::range(from, to));
    }
    let (from, to) = (from as usize, to as usize);
    Ok(match object {
        Val::List(items) => Val::list(items[from..to].to_vec()),
        Val::Str(s) => Val::string(utf16_slice(s, from, to)),
        Val::Path(segments) => Val::path(segments[from..to].to_vec()),
        _ => unreachable!("checked above"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        assert_eq!(crc32(0xEDB8_8320, b"abc"), 0x3524_41C2);
        assert_eq!(crc32(0x82F6_3B78, b"abc"), 0x364B_3FB7);
        assert_eq!(
            hash("hashing.md5", b"abc"),
            [
                0x90, 0x01, 0x50, 0x98, 0x3C, 0xD2, 0x4F, 0xB0, 0xD6, 0x96, 0x3F, 0x7D, 0x28, 0xE1,
                0x7F, 0x72
            ]
        );
    }

    #[test]
    fn base64() {
        assert_eq!(base64_url(b"\xfb\xff"), "-_8=");
        assert_eq!(base64_url(b"\xFB\xEF\xBE"), "----");
        assert_eq!(base64_url(b"a"), "YQ==");
        assert_eq!(base64_url(b""), "");
    }

    #[test]
    fn rounding() {
        assert_eq!(java_round(2.5), 3);
        assert_eq!(java_round(-2.5), -2);
        assert_eq!(java_round(-0.5), 0);
        assert_eq!(java_round(0.499_999_999_999_999_94), 0);
        assert_eq!(java_round(f64::NAN), 0);
        assert_eq!(java_round(1e300), i64::MAX);
    }
}
