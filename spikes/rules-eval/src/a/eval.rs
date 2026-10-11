//! Evaluator A: values, static checks and a tree-walking interpreter with Rules semantics.

use super::parse::{Expr, Kind};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    List(Rc<Vec<V>>),
    Map(Rc<Vec<(V, V)>>),
    Set(Rc<Vec<V>>),
    /// Seconds and nanoseconds since the Unix epoch, UTC.
    Ts(i64, u32),
    /// Nanoseconds.
    Dur(i128),
    Path(Rc<Vec<String>>),
    LatLng(f64, f64),
    /// Added, removed, changed and unchanged keys.
    Diff(Rc<[Vec<V>; 4]>),
}

/// An evaluation error: the message and the byte offset of the node that raised it.
#[derive(Debug, Clone)]
pub struct EvalErr(pub String, pub usize);
type R = Result<V, EvalErr>;

pub type Vars = HashMap<String, V>;

#[rustfmt::skip]
const GLOBALS: &[&str] = &[
    "int", "float", "string", "path", "math.abs", "math.ceil", "math.floor", "math.round",
    "math.sqrt", "math.pow", "math.isNaN", "math.isInfinite", "duration.value", "duration.time",
    "duration.abs", "timestamp.value", "timestamp.date", "latlng.value", "hashing.sha256",
];
#[rustfmt::skip]
const METHODS: &[&str] = &[
    "size", "keys", "values", "get", "diff", "hasAll", "hasAny", "hasOnly", "concat", "removeAll",
    "join", "toSet", "difference", "union", "intersection", "matches", "lower", "upper", "trim",
    "split", "replace", "toUtf8", "year", "month", "day", "hours", "minutes", "seconds", "nanos",
    "toMillis", "dayOfWeek", "dayOfYear", "date", "time", "latitude", "longitude", "distance",
    "toBase64", "toHexString", "addedKeys", "removedKeys", "changedKeys", "unchangedKeys",
    "affectedKeys",
];

/// The checks the Rules compiler makes that need no types: names must resolve, functions and
/// methods must exist, map literal keys must be strings.
pub fn check(e: &Expr, vars: &Vars) -> Result<(), String> {
    let mut err = None;
    walk(e, &mut |e| {
        let bad = match &e.kind {
            Kind::Ident(n) if !vars.contains_key(n) => Some(format!("unknown variable '{n}'")),
            Kind::Call(None, f, _) if !GLOBALS.contains(&f.as_str()) => {
                Some(format!("unknown function '{f}'"))
            }
            Kind::Call(Some(_), m, _) if !METHODS.contains(&m.as_str()) => {
                Some(format!("unknown method '{m}'"))
            }
            Kind::Map(entries) => entries
                .iter()
                .find(|(k, _)| {
                    !matches!(
                        k.kind,
                        Kind::Str(_)
                            | Kind::Ident(_)
                            | Kind::Select(..)
                            | Kind::Call(..)
                            | Kind::Index(..)
                    )
                })
                .map(|_| "map keys must be strings".to_string()),
            _ => None,
        };
        if err.is_none() {
            err = bad.map(|m| format!("{m} at {}", e.pos));
        }
    });
    err.map_or(Ok(()), Result::Err)?;
    static_type(e).map(|_| ())
}

/// The type of `e` where its literals and operators fix it (`None`: known only at run time),
/// and the type errors the Rules compiler reports for them: `'1' == 1`, `false && 32`,
/// `true ? 'a' : 1`, `[1] + [2]`, `b'' + b''`, `string(b'')`.
fn static_type(e: &Expr) -> Result<Option<&'static str>, String> {
    let bad = |m: &str| Result::Err(format!("type error at {}: {m}", e.pos));
    let numeric = |t: Option<&str>| matches!(t, Some("int" | "float"));
    let fits = |t: Option<&str>, want: &[&str]| t.is_none_or(|t| want.contains(&t));
    Ok(match &e.kind {
        Kind::Null => Some("null"),
        Kind::Bool(_) | Kind::Is(..) => Some("bool"),
        Kind::Int(_) => Some("int"),
        Kind::Float(_) => Some("float"),
        Kind::Str(_) => Some("string"),
        Kind::Bytes(_) => Some("bytes"),
        Kind::List(items) => {
            for i in items {
                static_type(i)?;
            }
            Some("list")
        }
        Kind::Map(entries) => {
            for (k, v) in entries {
                static_type(k)?;
                static_type(v)?;
            }
            Some("map")
        }
        Kind::Path(_) => Some("path"),
        Kind::Unary(op, x) => {
            let t = static_type(x)?;
            match *op {
                "!" if fits(t, &["bool"]) => Some("bool"),
                "-" if fits(t, &["int", "float"]) => t,
                _ => return bad(&format!("{op} on {}", t.unwrap_or("?"))),
            }
        }
        Kind::Binary(op, a, b) => {
            let (l, r) = (static_type(a)?, static_type(b)?);
            let comparable = l.is_none() || r.is_none() || l == r || (numeric(l) && numeric(r));
            match *op {
                "&&" | "||" if fits(l, &["bool"]) && fits(r, &["bool"]) => Some("bool"),
                "&&" | "||" => return bad(&format!("{op} needs bools")),
                "==" | "!=" | "<" | "<=" | ">" | ">=" if comparable => Some("bool"),
                "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                    return bad(&format!("{} {op} {}", l.unwrap(), r.unwrap()));
                }
                "in" => Some("bool"),
                // Arithmetic: numbers, and strings for `+`. A list or bytes operand is an error
                // even when the other side is only known at run time (`resource.data.a + [1]`).
                _ => {
                    let arith: &[&str] = if *op == "+" {
                        &["int", "float", "string"]
                    } else {
                        &["int", "float"]
                    };
                    match (l, r) {
                        _ if !fits(l, arith) || !fits(r, arith) => {
                            return bad(&format!("{} {op} {}", l.unwrap_or("?"), r.unwrap_or("?")));
                        }
                        (Some(x), Some(y)) if numeric(l) && numeric(r) => {
                            Some(if x == "float" || y == "float" {
                                "float"
                            } else {
                                "int"
                            })
                        }
                        (Some("string"), Some("string")) => Some("string"),
                        (Some(x), Some(y)) => return bad(&format!("{x} {op} {y}")),
                        _ => None,
                    }
                }
            }
        }
        Kind::Cond(c, a, b) => {
            if !fits(static_type(c)?, &["bool"]) {
                return bad("condition is not a bool");
            }
            match (static_type(a)?, static_type(b)?) {
                (Some(x), Some(y)) if x == y => Some(x),
                (Some(x), Some(y)) => return bad(&format!("branches are {x} and {y}")),
                _ => None,
            }
        }
        Kind::Call(target, f, args) => {
            if let Some(t) = target {
                static_type(t)?;
            }
            let types = args
                .iter()
                .map(static_type)
                .collect::<Result<Vec<_>, _>>()?;
            match (target.is_none(), f.as_str()) {
                (true, "string")
                    if !fits(
                        types[0],
                        &["int", "float", "bool", "null", "string", "path"],
                    ) =>
                {
                    return bad("string() of this type");
                }
                (true, "string") => Some("string"),
                (true, "int") => Some("int"),
                (true, "float") => Some("float"),
                _ => None,
            }
        }
        Kind::Select(x, _) => {
            static_type(x)?;
            None
        }
        Kind::Index(x, i) => {
            static_type(x)?;
            static_type(i)?;
            None
        }
        Kind::Slice(x, i, j) => {
            static_type(x)?;
            static_type(i)?;
            static_type(j)?;
            None
        }
        Kind::Ident(_) => None,
    })
}

fn walk(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    let kids: Vec<&Expr> = match &e.kind {
        Kind::List(v) => v.iter().collect(),
        Kind::Map(v) => v.iter().flat_map(|(a, b)| [a, b]).collect(),
        Kind::Path(segs) => segs.iter().filter_map(|s| s.as_ref().err()).collect(),
        Kind::Select(a, _) | Kind::Unary(_, a) | Kind::Is(a, _) => vec![a],
        Kind::Index(a, b) | Kind::Binary(_, a, b) => vec![a, b],
        Kind::Slice(a, b, c) | Kind::Cond(a, b, c) => vec![a, b, c],
        Kind::Call(t, _, args) => t.iter().map(|b| &**b).chain(args).collect(),
        _ => vec![],
    };
    for k in kids {
        walk(k, f);
    }
}

pub fn eval(e: &Expr, vars: &Vars) -> R {
    let fail = |m: String| EvalErr(m, e.pos);
    match &e.kind {
        Kind::Null => Ok(V::Null),
        Kind::Bool(b) => Ok(V::Bool(*b)),
        Kind::Int(i) => Ok(V::Int(*i)),
        Kind::Float(f) => Ok(V::Float(*f)),
        Kind::Str(s) => Ok(V::Str(s.as_str().into())),
        Kind::Bytes(b) => Ok(V::Bytes(b.as_slice().into())),
        Kind::Ident(n) => vars
            .get(n)
            .cloned()
            .ok_or_else(|| fail(format!("unknown variable '{n}'"))),
        Kind::List(items) => Ok(V::List(Rc::new(
            items
                .iter()
                .map(|i| eval(i, vars))
                .collect::<Result<_, _>>()?,
        ))),
        Kind::Map(entries) => {
            let mut out: Vec<(V, V)> = Vec::new();
            for (k, v) in entries {
                let k = eval(k, vars)?;
                if out.iter().any(|(x, _)| deep_eq(x, &k)) {
                    return Err(fail("duplicate map key".into()));
                }
                out.push((k, eval(v, vars)?));
            }
            Ok(V::Map(Rc::new(out)))
        }
        Kind::Path(segs) => {
            let mut out = Vec::new();
            for s in segs {
                match s {
                    Ok(s) => out.push(s.clone()),
                    Result::Err(x) => match eval(x, vars)? {
                        V::Str(s) => out.push(s.to_string()),
                        V::Path(p) => out.extend(p.iter().cloned()),
                        v => out.push(to_string(&v).map_err(fail)?),
                    },
                }
            }
            Ok(V::Path(Rc::new(out)))
        }
        Kind::Select(target, field) => match eval(target, vars)? {
            V::Map(m) => lookup(&m, field)
                .ok_or_else(|| fail(format!("Property {field} is undefined on object."))),
            v => Err(fail(format!("no field '{field}' on {}", type_name(&v)))),
        },
        Kind::Index(target, idx) => index(eval(target, vars)?, eval(idx, vars)?).map_err(fail),
        Kind::Slice(target, i, j) => {
            slice(eval(target, vars)?, eval(i, vars)?, eval(j, vars)?).map_err(fail)
        }
        Kind::Unary(op, x) => match (*op, eval(x, vars)?) {
            ("!", V::Bool(b)) => Ok(V::Bool(!b)),
            ("-", V::Int(i)) => i
                .checked_neg()
                .map(V::Int)
                .ok_or_else(|| fail("integer overflow".into())),
            ("-", V::Float(f)) => Ok(V::Float(-f)),
            (op, v) => Err(fail(format!("no operator {op} for {}", type_name(&v)))),
        },
        Kind::Binary("&&", a, b) | Kind::Binary("||", a, b) => {
            let and = matches!(e.kind, Kind::Binary("&&", ..));
            // Errors are absorbed when the other side decides: false && error, error || true.
            let l = eval(a, vars).and_then(|v| as_bool(v, a.pos));
            if let Ok(l) = &l
                && *l != and
            {
                return Ok(V::Bool(*l));
            }
            let r = eval(b, vars).and_then(|v| as_bool(v, b.pos));
            match (l, r) {
                (_, Ok(r)) if r != and => Ok(V::Bool(r)),
                (Ok(_), Ok(r)) => Ok(V::Bool(r)),
                (Result::Err(x), _) | (_, Result::Err(x)) => Err(x),
            }
        }
        Kind::Binary(op, a, b) => binary(op, eval(a, vars)?, eval(b, vars)?).map_err(fail),
        Kind::Is(x, t) => Ok(V::Bool(is_type(&eval(x, vars)?, t))),
        Kind::Cond(c, a, b) => {
            if as_bool(eval(c, vars)?, c.pos)? {
                eval(a, vars)
            } else {
                eval(b, vars)
            }
        }
        Kind::Call(None, f, args) => {
            let args: Vec<V> = args
                .iter()
                .map(|a| eval(a, vars))
                .collect::<Result<_, _>>()?;
            global(f, &args).map_err(fail)
        }
        Kind::Call(Some(target), m, args) => {
            let t = eval(target, vars)?;
            let args: Vec<V> = args
                .iter()
                .map(|a| eval(a, vars))
                .collect::<Result<_, _>>()?;
            method(t, m, &args).map_err(fail)
        }
    }
}

fn as_bool(v: V, pos: usize) -> Result<bool, EvalErr> {
    match v {
        V::Bool(b) => Ok(b),
        v => Err(EvalErr(
            format!("expected bool, got {}", type_name(&v)),
            pos,
        )),
    }
}

fn lookup(m: &[(V, V)], k: &str) -> Option<V> {
    m.iter()
        .find(|(x, _)| matches!(x, V::Str(s) if &**s == k))
        .map(|(_, v)| v.clone())
}

pub fn type_name(v: &V) -> &'static str {
    match v {
        V::Null => "null",
        V::Bool(_) => "bool",
        V::Int(_) => "int",
        V::Float(_) => "float",
        V::Str(_) => "string",
        V::Bytes(_) => "bytes",
        V::List(_) => "list",
        V::Map(_) => "map",
        V::Set(_) => "set",
        V::Ts(..) => "timestamp",
        V::Dur(_) => "duration",
        V::Path(_) => "path",
        V::LatLng(..) => "latlng",
        V::Diff(_) => "mapdiff",
    }
}

fn is_type(v: &V, t: &str) -> bool {
    match t {
        "number" => matches!(v, V::Int(_) | V::Float(_)),
        "set" | "mapdiff" => false,
        _ => type_name(v) == t,
    }
}

/// `==` in a condition: an int equals a float of the same value, but not inside lists or maps.
pub fn eq_top(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Int(x), V::Float(y)) | (V::Float(y), V::Int(x)) => (*x as f64) == *y,
        _ => deep_eq(a, b),
    }
}

pub fn deep_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Int(x), V::Int(y)) => x == y,
        (V::Float(x), V::Float(y)) => x == y,
        (V::Str(x), V::Str(y)) => x == y,
        (V::Bytes(x), V::Bytes(y)) => x == y,
        (V::List(x), V::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| deep_eq(a, b))
        }
        (V::Set(x), V::Set(y)) => {
            x.len() == y.len() && x.iter().all(|a| y.iter().any(|b| deep_eq(a, b)))
        }
        (V::Map(x), V::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, v2)| deep_eq(k, k2) && deep_eq(v, v2)))
        }
        (V::Ts(s, n), V::Ts(s2, n2)) => s == s2 && n == n2,
        (V::Dur(x), V::Dur(y)) => x == y,
        (V::Path(x), V::Path(y)) => x == y,
        (V::LatLng(a, b), V::LatLng(c, d)) => a == c && b == d,
        _ => false,
    }
}

/// Membership is strict: `1 in [1.0]` is false although `1 == 1.0`.
fn contains(items: &[V], v: &V) -> bool {
    items.iter().any(|x| deep_eq(x, v))
}

fn num(v: &V) -> Option<f64> {
    match v {
        V::Int(i) => Some(*i as f64),
        V::Float(f) => Some(*f),
        _ => None,
    }
}

fn binary(op: &str, a: V, b: V) -> Result<V, String> {
    use V::*;
    let overflow = || "integer overflow".to_string();
    let no = |a: &V, b: &V| format!("no operator {op} for {} and {}", type_name(a), type_name(b));
    Ok(match (op, &a, &b) {
        ("==", ..) => Bool(eq_top(&a, &b)),
        ("!=", ..) => Bool(!eq_top(&a, &b)),
        ("in", _, List(l)) | ("in", _, Set(l)) => Bool(contains(l, &a)),
        ("in", _, Map(m)) => Bool(m.iter().any(|(k, _)| eq_top(k, &a))),
        ("<" | "<=" | ">" | ">=", ..) => {
            let ord = match (&a, &b) {
                (Int(x), Int(y)) => x.cmp(y),
                (Str(x), Str(y)) => x.cmp(y),
                (Bytes(x), Bytes(y)) => x.cmp(y),
                (Ts(s, n), Ts(s2, n2)) => (s, n).cmp(&(s2, n2)),
                (Dur(x), Dur(y)) => x.cmp(y),
                _ => match (num(&a), num(&b)) {
                    (Some(x), Some(y)) => match x.partial_cmp(&y) {
                        Some(o) => o,
                        None => return Ok(Bool(false)),
                    },
                    _ => return Result::Err(no(&a, &b)),
                },
            };
            Bool(match op {
                "<" => ord.is_lt(),
                "<=" => ord.is_le(),
                ">" => ord.is_gt(),
                _ => ord.is_ge(),
            })
        }
        ("+", Int(x), Int(y)) => Int(x.checked_add(*y).ok_or_else(overflow)?),
        ("-", Int(x), Int(y)) => Int(x.checked_sub(*y).ok_or_else(overflow)?),
        ("*", Int(x), Int(y)) => Int(x.checked_mul(*y).ok_or_else(overflow)?),
        ("/", Int(_), Int(0)) | ("%", Int(_), Int(0)) => {
            return Result::Err("division by zero".into());
        }
        ("/", Int(x), Int(y)) => Int(x.checked_div(*y).ok_or_else(overflow)?),
        ("%", Int(x), Int(y)) => Int(x.checked_rem(*y).ok_or_else(overflow)?),
        ("+", Str(x), Str(y)) => Str(format!("{x}{y}").into()),
        ("+", Ts(..), Dur(d)) | ("+", Dur(d), Ts(..)) => {
            let t = if let Ts(..) = a { &a } else { &b };
            ts_add(t, *d)?
        }
        ("-", Ts(..), Dur(d)) => ts_add(&a, -d)?,
        ("-", Ts(s, n), Ts(s2, n2)) => {
            Dur((*s as i128 - *s2 as i128) * 1_000_000_000 + (*n as i128 - *n2 as i128))
        }
        ("+", Dur(x), Dur(y)) => Dur(x + y),
        ("-", Dur(x), Dur(y)) => Dur(x - y),
        ("+" | "-" | "*" | "/" | "%", ..) => match (num(&a), num(&b)) {
            (Some(x), Some(y)) => Float(match op {
                "+" => x + y,
                "-" => x - y,
                "*" => x * y,
                "/" => x / y,
                _ => x % y,
            }),
            _ => return Result::Err(no(&a, &b)),
        },
        _ => return Result::Err(no(&a, &b)),
    })
}

fn ts_add(t: &V, d: i128) -> Result<V, String> {
    let V::Ts(s, n) = t else { unreachable!() };
    let total = *s as i128 * 1_000_000_000 + *n as i128 + d;
    Ok(V::Ts(
        total.div_euclid(1_000_000_000) as i64,
        total.rem_euclid(1_000_000_000) as u32,
    ))
}

fn index(t: V, i: V) -> Result<V, String> {
    match (&t, &i) {
        (V::List(l), V::Int(i)) => usize::try_from(*i)
            .ok()
            .and_then(|i| l.get(i).cloned())
            .ok_or_else(|| "index out of range".into()),
        (V::Map(m), V::Str(k)) => {
            lookup(m, k).ok_or_else(|| format!("Property {k} is undefined on object."))
        }
        (V::Map(m), k) => m
            .iter()
            .find(|(x, _)| eq_top(x, k))
            .map(|(_, v)| v.clone())
            .ok_or_else(|| "no such key".into()),
        (V::Path(p), V::Int(i)) => usize::try_from(*i)
            .ok()
            .and_then(|i| p.get(i))
            .map(|s| V::Str(s.as_str().into()))
            .ok_or_else(|| "index out of range".into()),
        (V::Str(s), V::Int(i)) => usize::try_from(*i)
            .ok()
            .and_then(|i| s.chars().nth(i))
            .map(|c| V::Str(c.to_string().into()))
            .ok_or_else(|| "index out of range".into()),
        _ => Result::Err(format!(
            "cannot index {} with {}",
            type_name(&t),
            type_name(&i)
        )),
    }
}

fn slice(t: V, i: V, j: V) -> Result<V, String> {
    let (V::Int(i), V::Int(j)) = (i, j) else {
        return Result::Err("slice bounds must be ints".into());
    };
    let range = |len: usize| -> Result<std::ops::Range<usize>, String> {
        let (i, j) = (
            usize::try_from(i).map_err(|_| "bad slice")?,
            usize::try_from(j).map_err(|_| "bad slice")?,
        );
        if i <= j && j <= len {
            Ok(i..j)
        } else {
            Result::Err("slice out of range".into())
        }
    };
    match t {
        V::List(l) => Ok(V::List(Rc::new(l[range(l.len())?].to_vec()))),
        V::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            Ok(V::Str(
                chars[range(chars.len())?].iter().collect::<String>().into(),
            ))
        }
        t => Result::Err(format!("cannot slice {}", type_name(&t))),
    }
}

pub fn to_string(v: &V) -> Result<String, String> {
    Ok(match v {
        V::Null => "null".into(),
        V::Bool(b) => b.to_string(),
        V::Int(i) => i.to_string(),
        V::Float(f) => format!("{f:?}"),
        V::Str(s) => s.to_string(),
        V::Path(p) => p.iter().map(|s| format!("/{s}")).collect(),
        v => return Result::Err(format!("no string() for {}", type_name(v))),
    })
}

fn items(v: &V) -> Result<&[V], String> {
    match v {
        V::List(l) | V::Set(l) => Ok(l),
        v => Result::Err(format!("expected a list or set, got {}", type_name(v))),
    }
}

fn to_set(items: &[V]) -> V {
    let mut out: Vec<V> = Vec::new();
    for i in items {
        if !contains(&out, i) {
            out.push(i.clone());
        }
    }
    V::Set(Rc::new(out))
}

fn str_arg(args: &[V], i: usize) -> Result<&str, String> {
    match args.get(i) {
        Some(V::Str(s)) => Ok(s),
        _ => Result::Err("expected a string argument".into()),
    }
}

fn int_arg(args: &[V], i: usize) -> Result<i64, String> {
    match args.get(i) {
        Some(V::Int(n)) => Ok(*n),
        _ => Result::Err("expected an int argument".into()),
    }
}

fn regex(re: &str, whole: bool) -> Result<regex::Regex, String> {
    regex::Regex::new(&if whole {
        format!("^(?:{re})$")
    } else {
        re.to_string()
    })
    .map_err(|e| e.to_string())
}

fn global(f: &str, args: &[V]) -> Result<V, String> {
    use V::*;
    let a0 = args.first().cloned().unwrap_or(Null);
    let float1 = |op: fn(f64) -> f64| {
        num(&a0)
            .map(op)
            .ok_or_else(|| "expected a number".to_string())
    };
    Ok(match (f, args.len()) {
        ("int", 1) => match &a0 {
            Int(i) => Int(*i),
            Float(x) => Int(*x as i64),
            Str(s) => Int(s.parse().map_err(|_| "cannot convert to int")?),
            _ => return Result::Err("cannot convert to int".into()),
        },
        ("float", 1) => match &a0 {
            Str(s) => Float(s.parse().map_err(|_| "cannot convert to float")?),
            v => Float(num(v).ok_or("cannot convert to float")?),
        },
        ("string", 1) => Str(to_string(&a0)?.into()),
        ("path", 1) => Path(Rc::new(
            str_arg(args, 0)?
                .split('/')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        )),
        ("math.abs", 1) => match a0 {
            Int(i) => Int(i.checked_abs().ok_or("integer overflow")?),
            _ => Float(float1(f64::abs)?),
        },
        ("math.ceil", 1) => Int(float1(f64::ceil)? as i64),
        ("math.floor", 1) => Int(float1(f64::floor)? as i64),
        ("math.round", 1) => Int(float1(f64::round)? as i64),
        ("math.sqrt", 1) => Float(float1(f64::sqrt)?),
        ("math.pow", 2) => Float(
            num(&a0)
                .zip(num(&args[1]))
                .map(|(x, y)| x.powf(y))
                .ok_or("expected numbers")?,
        ),
        ("math.isNaN", 1) => Bool(float1(|x| x)?.is_nan()),
        ("math.isInfinite", 1) => Bool(float1(|x| x)?.is_infinite()),
        ("duration.value", 2) => {
            let unit: i128 = match str_arg(args, 1)? {
                "w" => 604_800_000_000_000,
                "d" => 86_400_000_000_000,
                "h" => 3_600_000_000_000,
                "m" => 60_000_000_000,
                "s" => 1_000_000_000,
                "ms" => 1_000_000,
                "ns" => 1,
                u => return Result::Err(format!("unknown unit '{u}'")),
            };
            Dur(int_arg(args, 0)? as i128 * unit)
        }
        ("duration.time", 4) => {
            let p: Vec<i128> = (0..4)
                .map(|i| int_arg(args, i).map(i128::from))
                .collect::<Result<_, _>>()?;
            Dur(((p[0] * 60 + p[1]) * 60 + p[2]) * 1_000_000_000 + p[3])
        }
        ("duration.abs", 1) => match a0 {
            Dur(d) => Dur(d.abs()),
            _ => return Result::Err("expected a duration".into()),
        },
        ("timestamp.value", 1) => {
            let ms = int_arg(args, 0)?;
            Ts(
                ms.div_euclid(1000),
                (ms.rem_euclid(1000) * 1_000_000) as u32,
            )
        }
        ("timestamp.date", 3) => Ts(
            days_from_civil(int_arg(args, 0)?, int_arg(args, 1)?, int_arg(args, 2)?) * 86_400,
            0,
        ),
        ("latlng.value", 2) => LatLng(
            num(&a0).ok_or("expected a number")?,
            num(&args[1]).ok_or("expected a number")?,
        ),
        ("hashing.sha256", 1) => {
            use sha2::Digest;
            let data: Vec<u8> = match &a0 {
                Str(s) => s.as_bytes().to_vec(),
                Bytes(b) => b.to_vec(),
                _ => return Result::Err("expected string or bytes".into()),
            };
            Bytes(sha2::Sha256::digest(&data).to_vec().into())
        }
        _ => return Result::Err(format!("no overload for {f} with {} arguments", args.len())),
    })
}

fn method(t: V, m: &str, args: &[V]) -> Result<V, String> {
    use V::*;
    let a0 = args.first();
    let keys_set = |i: usize, d: &[Vec<V>; 4]| Set(Rc::new(d[i].clone()));
    Ok(match (m, &t, args.len()) {
        ("size", Str(s), 0) => Int(s.chars().count() as i64),
        ("size", Bytes(b), 0) => Int(b.len() as i64),
        ("size", List(l) | Set(l), 0) => Int(l.len() as i64),
        ("size", Map(x), 0) => Int(x.len() as i64),
        ("keys", Map(x), 0) => List(Rc::new(x.iter().map(|(k, _)| k.clone()).collect())),
        ("values", Map(x), 0) => List(Rc::new(x.iter().map(|(_, v)| v.clone()).collect())),
        ("get", Map(x), 2) => {
            let path: Vec<V> = match &args[0] {
                List(p) => p.to_vec(),
                k => vec![k.clone()],
            };
            let mut cur = Map(x.clone());
            for k in &path {
                match (&cur, k) {
                    (Map(m), Str(k)) => match lookup(m, k) {
                        Some(v) => cur = v,
                        None => return Ok(args[1].clone()),
                    },
                    _ => return Ok(args[1].clone()),
                }
            }
            cur
        }
        ("diff", Map(x), 1) => {
            let Some(Map(y)) = a0 else {
                return Result::Err("diff() takes a map".into());
            };
            let mut d: [Vec<V>; 4] = Default::default();
            for (k, v) in x.iter() {
                match y.iter().find(|(k2, _)| deep_eq(k, k2)) {
                    None => d[0].push(k.clone()),
                    Some((_, v2)) if deep_eq(v, v2) => d[3].push(k.clone()),
                    Some(_) => d[2].push(k.clone()),
                }
            }
            for (k, _) in y.iter() {
                if !x.iter().any(|(k2, _)| deep_eq(k, k2)) {
                    d[1].push(k.clone());
                }
            }
            Diff(Rc::new(d))
        }
        ("addedKeys", Diff(d), 0) => keys_set(0, d),
        ("removedKeys", Diff(d), 0) => keys_set(1, d),
        ("changedKeys", Diff(d), 0) => keys_set(2, d),
        ("unchangedKeys", Diff(d), 0) => keys_set(3, d),
        ("affectedKeys", Diff(d), 0) => Set(Rc::new(
            d[0].iter().chain(&d[1]).chain(&d[2]).cloned().collect(),
        )),
        ("hasAll", List(l) | Set(l), 1) => Bool(items(&args[0])?.iter().all(|x| contains(l, x))),
        ("hasAny", List(l) | Set(l), 1) => Bool(items(&args[0])?.iter().any(|x| contains(l, x))),
        ("hasOnly", List(l) | Set(l), 1) => {
            let other = items(&args[0])?;
            Bool(l.iter().all(|x| contains(other, x)))
        }
        ("concat", List(l), 1) => {
            List(Rc::new(l.iter().chain(items(&args[0])?).cloned().collect()))
        }
        ("removeAll", List(l), 1) => {
            let other = items(&args[0])?;
            List(Rc::new(
                l.iter().filter(|x| !contains(other, x)).cloned().collect(),
            ))
        }
        ("join", List(l), 1) => {
            let parts: Vec<String> = l
                .iter()
                .map(|v| match v {
                    Str(s) => Ok(s.to_string()),
                    _ => Result::Err("join() needs strings".to_string()),
                })
                .collect::<Result<_, _>>()?;
            Str(parts.join(str_arg(args, 0)?).into())
        }
        ("toSet", List(l), 0) => to_set(l),
        ("difference", Set(l), 1) => {
            let other = items(&args[0])?;
            Set(Rc::new(
                l.iter().filter(|x| !contains(other, x)).cloned().collect(),
            ))
        }
        ("union", Set(l), 1) => to_set(
            &l.iter()
                .chain(items(&args[0])?)
                .cloned()
                .collect::<Vec<_>>(),
        ),
        ("intersection", Set(l), 1) => {
            let other = items(&args[0])?;
            Set(Rc::new(
                l.iter().filter(|x| contains(other, x)).cloned().collect(),
            ))
        }
        ("matches", Str(s), 1) => Bool(regex(str_arg(args, 0)?, true)?.is_match(s)),
        ("lower", Str(s), 0) => Str(s.to_lowercase().into()),
        ("upper", Str(s), 0) => Str(s.to_uppercase().into()),
        ("trim", Str(s), 0) => Str(s.trim().into()),
        ("split", Str(s), 1) => List(Rc::new(
            regex(str_arg(args, 0)?, false)?
                .split(s)
                .map(|p| Str(p.into()))
                .collect(),
        )),
        ("replace", Str(s), 2) => Str(regex(str_arg(args, 0)?, false)?
            .replace_all(s, str_arg(args, 1)?)
            .into_owned()
            .into()),
        ("toUtf8", Str(s), 0) => Bytes(s.as_bytes().into()),
        ("toBase64", Bytes(b), 0) => Str(base64(b).into()),
        ("toHexString", Bytes(b), 0) => Str(b
            .iter()
            .map(|x| format!("{x:02X}"))
            .collect::<String>()
            .into()),
        ("lower", ..) | ("upper", ..) | ("trim", ..) => {
            return Result::Err(format!("{m}() needs a string"));
        }
        (
            "year" | "month" | "day" | "hours" | "minutes" | "seconds" | "nanos" | "toMillis"
            | "dayOfWeek" | "dayOfYear" | "date" | "time",
            Ts(s, n),
            0,
        ) => {
            let (days, secs) = (s.div_euclid(86_400), s.rem_euclid(86_400));
            let (y, mo, d) = civil_from_days(days);
            match m {
                "year" => Int(y),
                "month" => Int(mo),
                "day" => Int(d),
                "hours" => Int(secs / 3600),
                "minutes" => Int(secs / 60 % 60),
                "seconds" => Int(secs % 60),
                "nanos" => Int(*n as i64),
                "toMillis" => Int(s * 1000 + *n as i64 / 1_000_000),
                "dayOfWeek" => Int((days + 3).rem_euclid(7) + 1),
                "dayOfYear" => Int(days - days_from_civil(y, 1, 1) + 1),
                "date" => Ts(days * 86_400, 0),
                _ => Dur(secs as i128 * 1_000_000_000 + *n as i128),
            }
        }
        ("seconds", Dur(d), 0) => Int((d / 1_000_000_000) as i64),
        ("nanos", Dur(d), 0) => Int((d % 1_000_000_000) as i64),
        ("latitude", LatLng(lat, _), 0) => Float(*lat),
        ("longitude", LatLng(_, lng), 0) => Float(*lng),
        ("distance", LatLng(a, b), 1) => {
            let Some(LatLng(c, d)) = a0 else {
                return Result::Err("distance() takes a latlng".into());
            };
            let (p1, p2, dl) = (a.to_radians(), c.to_radians(), (d - b).to_radians());
            let h =
                ((p2 - p1) / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
            Float(2.0 * 6_378_137.0 * h.sqrt().asin())
        }
        _ => {
            return Result::Err(format!(
                "no method {m}() on {} with {} arguments",
                type_name(&t),
                args.len()
            ));
        }
    })
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                T[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}
