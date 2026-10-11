//! Binary size probe: the shared code and serde_json, no evaluator (see README).
#![allow(dead_code)]

#[path = "../common.rs"]
mod common;

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    println!("{:?}", common::float_json(src.len() as f64).to_string());
}
