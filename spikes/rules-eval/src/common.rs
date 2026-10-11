//! What the evaluators share with the harness: their input and output types.

use serde_json::{Value as Json, json};

/// A variable's value, as every evaluator receives it.
#[derive(Clone, Debug)]
pub enum Input {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Input>),
    Map(Vec<(Input, Input)>),
    Ts(i64, u32),
    Path(Vec<String>),
    LatLng(f64, f64),
}

#[derive(Debug)]
pub enum Failure {
    Compile(String),
    Eval(String),
}

pub fn float_json(f: f64) -> Json {
    if f.is_nan() {
        json!({"double": "NaN"})
    } else if f.is_infinite() {
        json!({"double": if f > 0.0 { "Infinity" } else { "-Infinity" }})
    } else {
        json!({"double": f})
    }
}

pub const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(b: &[u8]) -> String {
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                B64[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}
