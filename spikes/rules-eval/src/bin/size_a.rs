//! Binary size probe: evaluator A alone (see README).
#![allow(dead_code)]

#[path = "../a/mod.rs"]
mod a;
#[path = "../common.rs"]
mod common;

use common::{Failure, Input, base64, float_json};

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    println!(
        "{:?}",
        a::run(&src, &[("x".into(), Input::Int(1))]).map(|j| j.to_string())
    );
}
