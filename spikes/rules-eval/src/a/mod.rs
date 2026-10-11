//! Evaluator A: a hand-written parser and tree-walking interpreter for Rules conditions.

mod eval;
pub mod parse;

use crate::{Failure, Input};
use eval::V;
use serde_json::{Value as Json, json};
use std::rc::Rc;

pub fn run(src: &str, vars: &[(String, Input)]) -> Result<Json, Failure> {
    let expr = parse::parse(src).map_err(Failure::Compile)?;
    let vars: eval::Vars = vars
        .iter()
        .map(|(k, v)| (k.clone(), from_input(v)))
        .collect();
    eval::check(&expr, &vars).map_err(Failure::Compile)?;
    match eval::eval(&expr, &vars) {
        Ok(v) => Ok(to_json(&v)),
        Err(eval::EvalErr(msg, pos)) => Err(Failure::Eval(format!("{msg} (at {pos})"))),
    }
}

/// A's static checks, for C.
pub fn check(expr: &parse::Expr, vars: &[(String, Input)]) -> Result<(), String> {
    eval::check(
        expr,
        &vars
            .iter()
            .map(|(k, v)| (k.clone(), from_input(v)))
            .collect(),
    )
}

fn from_input(i: &Input) -> V {
    match i {
        Input::Null => V::Null,
        Input::Bool(b) => V::Bool(*b),
        Input::Int(i) => V::Int(*i),
        Input::Float(f) => V::Float(*f),
        Input::Str(s) => V::Str(s.as_str().into()),
        Input::Bytes(b) => V::Bytes(b.as_slice().into()),
        Input::List(l) => V::List(Rc::new(l.iter().map(from_input).collect())),
        Input::Map(m) => V::Map(Rc::new(
            m.iter()
                .map(|(k, v)| (from_input(k), from_input(v)))
                .collect(),
        )),
        Input::Ts(s, n) => V::Ts(*s, *n),
        Input::Path(p) => V::Path(Rc::new(p.clone())),
        Input::LatLng(a, b) => V::LatLng(*a, *b),
    }
}

fn to_json(v: &V) -> Json {
    match v {
        V::Null => json!({"null": null}),
        V::Bool(b) => json!({"bool": b}),
        V::Int(i) => json!({"int": i.to_string()}),
        V::Float(f) => crate::float_json(*f),
        V::Str(s) => json!({"string": &**s}),
        V::Bytes(b) => json!({"bytes": crate::base64(b)}),
        V::List(l) => json!({"list": l.iter().map(to_json).collect::<Vec<_>>()}),
        V::Map(m) => {
            json!({"map": m.iter().map(|(k, v)| json!([to_json(k), to_json(v)])).collect::<Vec<_>>()})
        }
        v => json!({"other": eval::type_name(v)}),
    }
}
