//! Evaluator B: cel-rust (`cel` 0.15) with the Rules library added as context functions and the
//! Rules types (path, set, map diff, latlng) as custom values. Parsing is cel-rust's CEL grammar,
//! operators are cel-rust's. Also `run_stock`: cel-rust's standard environment, unextended.

use crate::{Failure, Input};
use cel::common::traits::{Container, Indexer};
use cel::common::types::{
    CelBool, CelBytes, CelDouble, CelDuration, CelInt, CelList, CelMap, CelMapKey, CelNull,
    CelString, CelTimestamp, Kind, Type,
};
use cel::common::value::{CowVal, StaticVal, Val};
use cel::{Context, Env, ExecutionError, FunctionContext, Value};
use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Timelike};
use serde_json::{Value as Json, json};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

type Res<'c, 'v> = Result<CowVal<'c, 'v>, ExecutionError>;
type F = Box<dyn for<'c, 'v> Fn(&mut FunctionContext<'c, 'v>) -> Res<'c, 'v> + Send + Sync>;

// --- Rules types as cel-rust values ----------------------------------------------------------

static PATH: Type = Type::new_unspecified_type("path");
static SET: Type = Type::new_unspecified_type("set");
static LATLNG: Type = Type::new_unspecified_type("latlng");
static DIFF: Type = Type::new_unspecified_type("mapdiff");

#[derive(Debug, Clone, PartialEq)]
pub struct RPath(Vec<String>);
#[derive(Debug, Clone)]
pub struct RSet(Vec<Box<dyn Val>>);
#[derive(Debug, Clone, PartialEq)]
pub struct RLatLng(f64, f64);
/// Added, removed, changed and unchanged keys.
#[derive(Debug, Clone)]
pub struct RDiff([Vec<String>; 4]);

macro_rules! custom_val {
    ($t:ty, $ty:ident, $eq:expr, $($extra:item)*) => {
        impl Val for $t {
            fn get_type(&self) -> &Type {
                &$ty
            }
            fn cel_type() -> &'static Type {
                &$ty
            }
            fn equals(&self, other: &dyn Val) -> bool {
                other.downcast_ref::<$t>().is_some_and(|o| $eq(self, o))
            }
            fn clone_as_boxed<'v>(&self) -> Box<dyn Val + 'v> where Self: 'v {
                Box::new(self.clone())
            }
            fn as_any(&self) -> Option<&dyn Any> {
                Some(self)
            }
            $($extra)*
        }
        impl StaticVal for $t {}
    };
}

custom_val!(
    RPath,
    PATH,
    |a: &RPath, b: &RPath| a == b,
    fn as_indexer<'b, 'v>(&'b self) -> Option<&'b (dyn Indexer + 'v)>
    where
        Self: 'v,
    {
        Some(self)
    }
);
custom_val!(
    RSet,
    SET,
    |a: &RSet, b: &RSet| a.0.len() == b.0.len()
        && a.0.iter().all(|x| b.0.iter().any(|y| x.equals(y.as_ref()))),
    fn as_container(&self) -> Option<&dyn Container> {
        Some(self)
    }
);
custom_val!(RLatLng, LATLNG, |a: &RLatLng, b: &RLatLng| a == b,);
custom_val!(RDiff, DIFF, |a: &RDiff, b: &RDiff| a.0 == b.0,);

impl Indexer for RPath {
    fn get<'b, 'v>(&'b self, idx: &dyn Val) -> Result<CowVal<'b, 'v>, ExecutionError>
    where
        Self: 'v,
    {
        let i = idx
            .downcast_ref::<CelInt>()
            .ok_or_else(|| ExecutionError::function_error("[]", "path index must be an int"))?;
        let seg = usize::try_from(*i.inner()).ok().and_then(|i| self.0.get(i));
        seg.map(|s| CowVal::owned(CelString::from(s.clone())))
            .ok_or_else(|| ExecutionError::IndexOutOfBounds(Value::Int(*i.inner())))
    }
    fn steal<'v>(self: Box<Self>, idx: &dyn Val) -> Result<Box<dyn Val + 'v>, ExecutionError>
    where
        Self: 'v,
    {
        self.get(idx).map(CowVal::into_owned)
    }
}

impl Container for RSet {
    fn contains(&self, v: &dyn Val) -> Result<bool, ExecutionError> {
        Ok(self.0.iter().any(|x| x.equals(v)))
    }
}

// --- Inputs -------------------------------------------------------------------------------------

fn ts(secs: i64, nanos: u32) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(0)
        .unwrap()
        .timestamp_opt(secs, nanos)
        .unwrap()
}

/// `stock` has no Rules types: paths become strings and latlngs maps.
fn to_val(i: &Input, stock: bool) -> Box<dyn Val> {
    match i {
        Input::Null => Box::new(CelNull),
        Input::Bool(b) => Box::new(CelBool::from(*b)),
        Input::Int(n) => Box::new(CelInt::from(*n)),
        Input::Float(f) => Box::new(CelDouble::from(*f)),
        Input::Str(s) => Box::new(CelString::from(s.clone())),
        Input::Bytes(b) => Box::new(CelBytes::from(b.clone())),
        Input::List(l) => Box::new(CelList::from(
            l.iter().map(|x| to_val(x, stock)).collect::<Vec<_>>(),
        )),
        Input::Map(m) => Box::new(CelMap::from(
            m.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Input::Str(s) => CelMapKey::String(CelString::from(s.clone())),
                        Input::Int(n) => CelMapKey::Int(CelInt::from(*n)),
                        Input::Bool(b) => CelMapKey::Bool(CelBool::from(*b)),
                        k => panic!("map key {k:?}"),
                    };
                    (key, to_val(v, stock))
                })
                .collect::<HashMap<_, _>>(),
        )),
        Input::Ts(s, n) => Box::new(CelTimestamp::from(ts(*s, *n))),
        Input::Path(p) if stock => Box::new(CelString::from(
            p.iter().map(|s| format!("/{s}")).collect::<String>(),
        )),
        Input::Path(p) => Box::new(RPath(p.clone())),
        Input::LatLng(a, b) if stock => to_val(
            &Input::Map(vec![
                (Input::Str("latitude".into()), Input::Float(*a)),
                (Input::Str("longitude".into()), Input::Float(*b)),
            ]),
            true,
        ),
        Input::LatLng(a, b) => Box::new(RLatLng(*a, *b)),
    }
}

pub fn to_json(v: &Value) -> Json {
    match v {
        Value::Null => json!({"null": null}),
        Value::Bool(b) => json!({"bool": b}),
        Value::Int(i) => json!({"int": i.to_string()}),
        Value::UInt(u) => json!({"uint": u.to_string()}),
        Value::Float(f) => crate::float_json(*f),
        Value::String(s) => json!({"string": s.as_str()}),
        Value::Bytes(b) => json!({"bytes": crate::base64(b)}),
        Value::List(l) => json!({"list": l.iter().map(to_json).collect::<Vec<_>>()}),
        Value::Map(m) => json!({"map": m.map.iter().map(|(k, v)| {
            let k = match k {
                cel::objects::Key::Int(i) => json!({"int": i.to_string()}),
                cel::objects::Key::Uint(u) => json!({"uint": u.to_string()}),
                cel::objects::Key::Bool(b) => json!({"bool": b}),
                cel::objects::Key::String(s) => json!({"string": s.as_str()}),
            };
            json!([k, to_json(v)])
        }).collect::<Vec<_>>()}),
        v => json!({"other": format!("{v:?}")}),
    }
}

pub fn run_stock(src: &str, vars: &[(String, Input)]) -> Result<Json, Failure> {
    let env = Arc::new(Env::stdlib());
    let program = env
        .compile(src)
        .map_err(|e| Failure::Compile(e.to_string()))?;
    let mut ctx = Context::with_env(env);
    for (k, v) in vars {
        ctx.add_variable_as_val(k.as_str(), to_val(v, true));
    }
    program
        .execute(&ctx)
        .map(|v| to_json(&v))
        .map_err(|e| Failure::Eval(e.to_string()))
}

pub fn run(src: &str, vars: &[(String, Input)]) -> Result<Json, Failure> {
    let env = Arc::new(Env::default());
    let program = env
        .compile(src)
        .map_err(|e| Failure::Compile(e.to_string()))?;
    let ctx = context(env, vars);
    program
        .execute(&ctx)
        .map(|v| to_json(&v))
        .map_err(|e| Failure::Eval(e.to_string()))
}

/// The Rules library on an environment without CEL's standard functions (they would collide
/// with Rules functions of the same name but other semantics, such as `matches`).
pub fn context(env: Arc<Env>, vars: &[(String, Input)]) -> Context<'static, 'static> {
    let mut ctx = Context::with_env(env);
    for (k, v) in vars {
        ctx.add_variable_as_val(k.as_str(), to_val(v, false));
    }
    for name in NAMES {
        let f: F = Box::new(move |ftx| rules(name, ftx));
        ctx.add_function(name, f).unwrap();
    }
    ctx
}

#[rustfmt::skip]
const NAMES: &[&str] = &[
    "int", "float", "string", "path", "math.abs", "math.ceil", "math.floor", "math.round",
    "math.sqrt", "math.pow", "math.isNaN", "math.isInfinite", "duration.value", "duration.time",
    "duration.abs", "timestamp.value", "timestamp.date", "latlng.value", "hashing.sha256", "size",
    "keys", "values", "get", "diff", "hasAll", "hasAny", "hasOnly", "concat", "removeAll", "join",
    "toSet", "difference", "union", "intersection", "matches", "lower", "upper", "trim", "split",
    "replace", "toUtf8", "year", "month", "day", "hours", "minutes", "seconds", "nanos", "toMillis",
    "dayOfWeek", "dayOfYear", "date", "time", "latitude", "longitude", "distance", "toBase64",
    "toHexString", "addedKeys", "removedKeys", "changedKeys", "unchangedKeys", "affectedKeys",
    "@is", "@path", "@slice",
];

// --- Accessors over `dyn Val` -------------------------------------------------------------------

fn int(v: &dyn Val) -> Option<i64> {
    v.downcast_ref::<CelInt>().map(|i| *i.inner())
}
fn num(v: &dyn Val) -> Option<f64> {
    int(v)
        .map(|i| i as f64)
        .or_else(|| v.downcast_ref::<CelDouble>().map(|d| *d.inner()))
}
fn text(v: &dyn Val) -> Option<&str> {
    v.downcast_ref::<CelString>().map(|s| s.inner())
}
fn items<'a>(v: &'a dyn Val) -> Option<&'a [Box<dyn Val + 'a>]> {
    v.downcast_ref::<CelList>()
        .map(|l| l.inner())
        .or_else(|| v.downcast_ref::<RSet>().map(|s| s.0.as_slice()))
}
fn map_keys(v: &dyn Val) -> Option<Vec<String>> {
    let m = v.downcast_ref::<CelMap>()?;
    Some(
        m.inner()
            .keys()
            .filter_map(|k| match k {
                CelMapKey::String(s) => Some(s.inner().to_string()),
                _ => None,
            })
            .collect(),
    )
}
fn map_get<'a>(v: &'a dyn Val, k: &str) -> Option<&'a dyn Val> {
    let m = v.downcast_ref::<CelMap>()?;
    m.inner()
        .iter()
        .find(|(key, _)| matches!(key, CelMapKey::String(s) if s.inner() == k))
        .map(|(_, v)| v.as_ref())
}
fn timestamp(v: &dyn Val) -> Option<DateTime<FixedOffset>> {
    v.downcast_ref::<CelTimestamp>().map(|t| *t.inner())
}
fn has(list: &[Box<dyn Val + '_>], v: &dyn Val) -> bool {
    list.iter().any(|x| x.equals(v))
}
/// A `'static` copy, which a set needs to hold a value: values passed to functions may borrow
/// the expression or the variables, so built-in ones go through `Value` and back.
fn owned(v: &dyn Val) -> Result<Box<dyn Val>, ExecutionError> {
    if let Some(p) = v.downcast_ref::<RPath>() {
        return Ok(Box::new(p.clone()));
    }
    if let Some(l) = v.downcast_ref::<RLatLng>() {
        return Ok(Box::new(l.clone()));
    }
    if let Some(s) = v.downcast_ref::<RSet>() {
        return Ok(Box::new(s.clone()));
    }
    Box::<dyn Val>::try_from(Value::try_from(v)?)
}
fn out<'c, 'v, T: Val + 'v>(v: T) -> Res<'c, 'v> {
    Ok(CowVal::owned(v))
}
fn str_list<'c, 'v>(v: Vec<String>) -> Res<'c, 'v> {
    out(CelList::from(
        v.into_iter()
            .map(|s| Box::new(CelString::from(s)) as Box<dyn Val>)
            .collect::<Vec<_>>(),
    ))
}
fn set_of(v: Vec<Box<dyn Val>>) -> RSet {
    let mut out: Vec<Box<dyn Val>> = Vec::new();
    for x in v {
        if !has(&out, x.as_ref()) {
            out.push(x);
        }
    }
    RSet(out)
}
fn strings_set(v: &[String]) -> RSet {
    RSet(
        v.iter()
            .map(|s| Box::new(CelString::from(s.clone())) as Box<dyn Val>)
            .collect(),
    )
}

// --- The Rules library --------------------------------------------------------------------------

/// `name` is the name the function was registered under: `ftx.name` is the name as called,
/// which for `math.abs(x)` is `abs`.
fn rules<'c, 'v>(name: &'static str, ftx: &mut FunctionContext<'c, 'v>) -> Res<'c, 'v> {
    let err = |m: &str| ExecutionError::function_error(name, m);
    let this = ftx.this.as_deref();
    let args: Vec<&dyn Val> = ftx.args.iter().map(|a| a.as_ref()).collect();
    let arg = |i: usize| args.get(i).copied().ok_or_else(|| err("missing argument"));
    let s_arg = |i: usize| arg(i).and_then(|a| text(a).ok_or_else(|| err("expected a string")));
    let i_arg = |i: usize| arg(i).and_then(|a| int(a).ok_or_else(|| err("expected an int")));
    let n_arg = |i: usize| arg(i).and_then(|a| num(a).ok_or_else(|| err("expected a number")));
    let re = |p: &str, whole: bool| {
        regex::Regex::new(&if whole {
            format!("^(?:{p})$")
        } else {
            p.to_string()
        })
        .map_err(|e| err(&e.to_string()))
    };
    match (name, this) {
        ("int", None) => {
            let a = arg(0)?;
            let v = int(a)
                .or_else(|| a.downcast_ref::<CelDouble>().map(|d| *d.inner() as i64))
                .or_else(|| text(a).and_then(|s| s.parse().ok()));
            out(CelInt::from(v.ok_or_else(|| err("cannot convert to int"))?))
        }
        ("float", None) => {
            let a = arg(0)?;
            out(CelDouble::from(
                num(a)
                    .or_else(|| text(a).and_then(|s| s.parse().ok()))
                    .ok_or_else(|| err("cannot convert to float"))?,
            ))
        }
        ("string", None) => {
            let a = arg(0)?;
            let s = if let Some(s) = text(a) {
                s.to_string()
            } else if let Some(i) = int(a) {
                i.to_string()
            } else if let Some(d) = a.downcast_ref::<CelDouble>() {
                format!("{:?}", d.inner())
            } else if let Some(b) = a.downcast_ref::<CelBool>() {
                b.inner().to_string()
            } else if a.downcast_ref::<CelNull>().is_some() {
                "null".into()
            } else if let Some(p) = a.downcast_ref::<RPath>() {
                p.0.iter().map(|s| format!("/{s}")).collect()
            } else {
                return Err(err("no string() for this type"));
            };
            out(CelString::from(s))
        }
        ("path", None) => out(RPath(
            s_arg(0)?
                .split('/')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        )),
        ("@path", None) => {
            let mut segs = Vec::new();
            for a in &args {
                match (text(*a), a.downcast_ref::<RPath>()) {
                    (Some(s), _) => segs.push(s.to_string()),
                    (_, Some(p)) => segs.extend(p.0.iter().cloned()),
                    _ => return Err(err("path segments must be strings")),
                }
            }
            out(RPath(segs))
        }
        ("@is", None) => {
            let (a, t) = (arg(0)?, s_arg(1)?);
            let (kind, type_name) = (a.get_type().kind(), a.get_type().name());
            let is = match t {
                "number" => matches!(kind, Kind::Int | Kind::Double),
                "int" => kind == Kind::Int,
                "float" => kind == Kind::Double,
                "string" => kind == Kind::String,
                "bool" => kind == Kind::Boolean,
                "bytes" => kind == Kind::Bytes,
                "list" => kind == Kind::List,
                "map" => kind == Kind::Map,
                "timestamp" => kind == Kind::Timestamp,
                "duration" => kind == Kind::Duration,
                "path" | "latlng" => type_name == t,
                _ => false,
            };
            out(CelBool::from(is))
        }
        ("@slice", None) => {
            let (t, i, j) = (arg(0)?, i_arg(1)?, i_arg(2)?);
            let (i, j) = (
                usize::try_from(i).map_err(|_| err("bad slice"))?,
                usize::try_from(j).map_err(|_| err("bad slice"))?,
            );
            if let Some(s) = text(t) {
                let chars: Vec<char> = s.chars().collect();
                if i > j || j > chars.len() {
                    return Err(err("slice out of range"));
                }
                out(CelString::from(chars[i..j].iter().collect::<String>()))
            } else if let Some(l) = items(t) {
                if i > j || j > l.len() {
                    return Err(err("slice out of range"));
                }
                out(CelList::from(
                    l[i..j]
                        .iter()
                        .map(|x| owned(x.as_ref()))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            } else {
                Err(err("cannot slice this type"))
            }
        }
        ("math.abs", None) => match int(arg(0)?) {
            Some(i) => out(CelInt::from(
                i.checked_abs().ok_or_else(|| err("overflow"))?,
            )),
            None => out(CelDouble::from(n_arg(0)?.abs())),
        },
        ("math.ceil", None) => out(CelInt::from(n_arg(0)?.ceil() as i64)),
        ("math.floor", None) => out(CelInt::from(n_arg(0)?.floor() as i64)),
        ("math.round", None) => out(CelInt::from(n_arg(0)?.round() as i64)),
        ("math.sqrt", None) => out(CelDouble::from(n_arg(0)?.sqrt())),
        ("math.pow", None) => out(CelDouble::from(n_arg(0)?.powf(n_arg(1)?))),
        ("math.isNaN", None) => out(CelBool::from(n_arg(0)?.is_nan())),
        ("math.isInfinite", None) => out(CelBool::from(n_arg(0)?.is_infinite())),
        ("duration.value", None) => {
            let n = i_arg(0)?;
            let d = match s_arg(1)? {
                "w" => chrono::Duration::weeks(n),
                "d" => chrono::Duration::days(n),
                "h" => chrono::Duration::hours(n),
                "m" => chrono::Duration::minutes(n),
                "s" => chrono::Duration::seconds(n),
                "ms" => chrono::Duration::milliseconds(n),
                "ns" => chrono::Duration::nanoseconds(n),
                _ => return Err(err("unknown unit")),
            };
            out(CelDuration::from(d))
        }
        ("duration.time", None) => out(CelDuration::from(
            chrono::Duration::hours(i_arg(0)?)
                + chrono::Duration::minutes(i_arg(1)?)
                + chrono::Duration::seconds(i_arg(2)?)
                + chrono::Duration::nanoseconds(i_arg(3)?),
        )),
        ("duration.abs", None) => {
            let d = arg(0)?
                .downcast_ref::<CelDuration>()
                .ok_or_else(|| err("expected a duration"))?;
            out(CelDuration::from(d.inner().abs()))
        }
        ("timestamp.value", None) => {
            let ms = i_arg(0)?;
            out(CelTimestamp::from(ts(
                ms.div_euclid(1000),
                (ms.rem_euclid(1000) * 1_000_000) as u32,
            )))
        }
        ("timestamp.date", None) => {
            let d = chrono::NaiveDate::from_ymd_opt(
                i_arg(0)? as i32,
                i_arg(1)? as u32,
                i_arg(2)? as u32,
            )
            .ok_or_else(|| err("bad date"))?;
            out(CelTimestamp::from(ts(
                d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                0,
            )))
        }
        ("latlng.value", None) => out(RLatLng(n_arg(0)?, n_arg(1)?)),
        ("hashing.sha256", None) => {
            use sha2::Digest;
            let a = arg(0)?;
            let data = text(a)
                .map(|s| s.as_bytes().to_vec())
                .or_else(|| a.downcast_ref::<CelBytes>().map(|b| b.inner().to_vec()));
            out(CelBytes::from(
                sha2::Sha256::digest(data.ok_or_else(|| err("expected string or bytes"))?).to_vec(),
            ))
        }
        ("size", Some(t)) => {
            let n = if let Some(s) = text(t) {
                s.chars().count()
            } else if let Some(b) = t.downcast_ref::<CelBytes>() {
                b.inner().len()
            } else if let Some(l) = items(t) {
                l.len()
            } else if let Some(m) = t.downcast_ref::<CelMap>() {
                m.inner().len()
            } else {
                return Err(err("no size() for this type"));
            };
            out(CelInt::from(n as i64))
        }
        ("keys", Some(t)) => str_list(map_keys(t).ok_or_else(|| err("keys() needs a map"))?),
        ("values", Some(t)) => {
            let m = t
                .downcast_ref::<CelMap>()
                .ok_or_else(|| err("values() needs a map"))?;
            out(CelList::from(
                m.inner()
                    .values()
                    .map(|v| owned(v.as_ref()))
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        ("get", Some(t)) => {
            let key = arg(0)?;
            let path: Vec<&str> = match items(key) {
                Some(l) => l
                    .iter()
                    .map(|k| text(k.as_ref()).ok_or_else(|| err("keys must be strings")))
                    .collect::<Result<_, _>>()?,
                None => vec![text(key).ok_or_else(|| err("keys must be strings"))?],
            };
            let mut cur: &dyn Val = t;
            for k in path {
                match map_get(cur, k) {
                    Some(v) => cur = v,
                    None => return Ok(CowVal::Owned(owned(arg(1)?)?)),
                }
            }
            Ok(CowVal::Owned(owned(cur)?))
        }
        ("diff", Some(t)) => {
            let other = arg(0)?;
            let (a, b) = (
                map_keys(t).ok_or_else(|| err("diff() needs maps"))?,
                map_keys(other).ok_or_else(|| err("diff() needs maps"))?,
            );
            let mut d: [Vec<String>; 4] = Default::default();
            for k in &a {
                match map_get(other, k) {
                    None => d[0].push(k.clone()),
                    Some(v) if v.equals(map_get(t, k).unwrap()) => d[3].push(k.clone()),
                    Some(_) => d[2].push(k.clone()),
                }
            }
            d[1] = b.into_iter().filter(|k| !a.contains(k)).collect();
            out(RDiff(d))
        }
        (
            "addedKeys" | "removedKeys" | "changedKeys" | "unchangedKeys" | "affectedKeys",
            Some(t),
        ) => {
            let d = &t
                .downcast_ref::<RDiff>()
                .ok_or_else(|| err("needs a map diff"))?
                .0;
            let keys: Vec<String> = match name {
                "addedKeys" => d[0].clone(),
                "removedKeys" => d[1].clone(),
                "changedKeys" => d[2].clone(),
                "unchangedKeys" => d[3].clone(),
                _ => d[0].iter().chain(&d[1]).chain(&d[2]).cloned().collect(),
            };
            out(strings_set(&keys))
        }
        (
            "hasAll" | "hasAny" | "hasOnly" | "concat" | "removeAll" | "difference" | "union"
            | "intersection",
            Some(t),
        ) => {
            let (l, o) = (
                items(t).ok_or_else(|| err("needs a list or set"))?,
                items(arg(0)?).ok_or_else(|| err("needs a list or set"))?,
            );
            let is_set = t.downcast_ref::<RSet>().is_some();
            let pick = |keep: bool| {
                l.iter()
                    .filter(|x| has(o, x.as_ref()) == keep)
                    .map(|x| owned(x.as_ref()))
                    .collect::<Result<Vec<_>, _>>()
            };
            match name {
                "hasAll" => out(CelBool::from(o.iter().all(|x| has(l, x.as_ref())))),
                "hasAny" => out(CelBool::from(o.iter().any(|x| has(l, x.as_ref())))),
                "hasOnly" => out(CelBool::from(l.iter().all(|x| has(o, x.as_ref())))),
                "concat" if !is_set => out(CelList::from(
                    l.iter()
                        .chain(o)
                        .map(|x| owned(x.as_ref()))
                        .collect::<Result<Vec<_>, _>>()?,
                )),
                "removeAll" if !is_set => out(CelList::from(pick(false)?)),
                "difference" if is_set => out(RSet(pick(false)?)),
                "intersection" if is_set => out(RSet(pick(true)?)),
                "union" if is_set => out(set_of(
                    l.iter()
                        .chain(o)
                        .map(|x| owned(x.as_ref()))
                        .collect::<Result<_, _>>()?,
                )),
                _ => Err(err("wrong receiver type")),
            }
        }
        ("join", Some(t)) => {
            let l = items(t).ok_or_else(|| err("join() needs a list"))?;
            let parts: Vec<&str> = l
                .iter()
                .map(|x| text(x.as_ref()).ok_or_else(|| err("join() needs strings")))
                .collect::<Result<_, _>>()?;
            out(CelString::from(parts.join(s_arg(0)?)))
        }
        ("toSet", Some(t)) => out(set_of(
            items(t)
                .ok_or_else(|| err("toSet() needs a list"))?
                .iter()
                .map(|x| owned(x.as_ref()))
                .collect::<Result<_, _>>()?,
        )),
        ("matches" | "lower" | "upper" | "trim" | "split" | "replace" | "toUtf8", Some(t)) => {
            let s = text(t).ok_or_else(|| err("needs a string"))?;
            match name {
                "matches" => out(CelBool::from(re(s_arg(0)?, true)?.is_match(s))),
                "lower" => out(CelString::from(s.to_lowercase())),
                "upper" => out(CelString::from(s.to_uppercase())),
                "trim" => out(CelString::from(s.trim().to_string())),
                "split" => str_list(re(s_arg(0)?, false)?.split(s).map(String::from).collect()),
                "replace" => out(CelString::from(
                    re(s_arg(0)?, false)?.replace_all(s, s_arg(1)?).into_owned(),
                )),
                _ => out(CelBytes::from(s.as_bytes().to_vec())),
            }
        }
        ("toBase64" | "toHexString", Some(t)) => {
            let b = t
                .downcast_ref::<CelBytes>()
                .ok_or_else(|| err("needs bytes"))?
                .inner();
            let s = if name == "toBase64" {
                crate::base64(b)
            } else {
                b.iter().map(|x| format!("{x:02X}")).collect()
            };
            out(CelString::from(s))
        }
        ("seconds" | "nanos", Some(t)) if t.downcast_ref::<CelDuration>().is_some() => {
            let d = t.downcast_ref::<CelDuration>().unwrap().inner();
            out(CelInt::from(if name == "seconds" {
                d.num_seconds()
            } else {
                d.subsec_nanos() as i64
            }))
        }
        (
            "year" | "month" | "day" | "hours" | "minutes" | "seconds" | "nanos" | "toMillis"
            | "dayOfWeek" | "dayOfYear" | "date" | "time",
            Some(t),
        ) => {
            let t = timestamp(t).ok_or_else(|| err("needs a timestamp"))?;
            let midnight = ts(
                t.date_naive()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp(),
                0,
            );
            match name {
                "date" => out(CelTimestamp::from(midnight)),
                "time" => out(CelDuration::from(t - midnight)),
                name => out(CelInt::from(match name {
                    "year" => t.year() as i64,
                    "month" => t.month() as i64,
                    "day" => t.day() as i64,
                    "hours" => t.hour() as i64,
                    "minutes" => t.minute() as i64,
                    "seconds" => t.second() as i64,
                    "nanos" => t.nanosecond() as i64,
                    "toMillis" => t.timestamp_millis(),
                    "dayOfWeek" => t.weekday().number_from_monday() as i64,
                    _ => t.ordinal() as i64,
                })),
            }
        }
        ("latitude" | "longitude" | "distance", Some(t)) => {
            let RLatLng(a, b) = t
                .downcast_ref::<RLatLng>()
                .ok_or_else(|| err("needs a latlng"))?
                .clone();
            match name {
                "latitude" => out(CelDouble::from(a)),
                "longitude" => out(CelDouble::from(b)),
                _ => {
                    let RLatLng(c, d) = arg(0)?
                        .downcast_ref::<RLatLng>()
                        .ok_or_else(|| err("needs a latlng"))?
                        .clone();
                    let (p1, p2, dl) = (a.to_radians(), c.to_radians(), (d - b).to_radians());
                    let h = ((p2 - p1) / 2.0).sin().powi(2)
                        + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
                    out(CelDouble::from(2.0 * 6_378_137.0 * h.sqrt().asin()))
                }
            }
        }
        _ => Err(err("no such overload")),
    }
}
