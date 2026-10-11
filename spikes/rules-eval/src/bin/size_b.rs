//! Binary size probe: evaluator B (cel-rust) alone (see README).
#![allow(dead_code)]

#[path = "../b.rs"]
mod b;
#[path = "../common.rs"]
mod common;

use common::{Failure, Input, base64, float_json};

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    println!(
        "{:?}",
        b::run(&src, &[("x".into(), Input::Int(1))]).map(|j| j.to_string())
    );
}
