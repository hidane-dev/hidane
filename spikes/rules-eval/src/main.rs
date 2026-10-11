//! Spike for #61: runs one test set through a hand-written evaluator (A), cel-rust with Rules
//! extensions (B), A's parser feeding cel-rust's interpreter (C) and stock cel-rust, and prints
//! pass rates.
//!
//! `cargo run --release` prints the tables; `cargo run --release -- failures <a|b|c|stock>`
//! lists the cases an evaluator fails.

mod a;
mod b;
mod c;

use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

mod common;

pub use common::{B64, Failure, Input, base64, float_json};

type Eval = fn(&str, &[(String, Input)]) -> Result<Json, Failure>;

const EVALUATORS: [(&str, Eval); 4] = [
    ("A", a::run),
    ("B", b::run),
    ("C", c::run),
    ("stock", b::run_stock),
];

fn unbase64(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        acc = acc << 6 | B64.iter().position(|x| *x == c).expect("base64") as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// A cel-spec value in the tagged form of cases/cel-spec.json.
fn from_tagged(j: &Json) -> Input {
    let (k, v) = j.as_object().unwrap().iter().next().unwrap();
    match k.as_str() {
        "int" => Input::Int(v.as_str().unwrap().parse().unwrap()),
        "double" => Input::Float(
            v.as_f64()
                .unwrap_or_else(|| v.as_str().unwrap().parse().unwrap()),
        ),
        "string" => Input::Str(v.as_str().unwrap().into()),
        "bytes" => Input::Bytes(unbase64(v.as_str().unwrap())),
        "bool" => Input::Bool(v.as_bool().unwrap()),
        "null" => Input::Null,
        "list" => Input::List(v.as_array().unwrap().iter().map(from_tagged).collect()),
        "map" => Input::Map(
            v.as_array()
                .unwrap()
                .iter()
                .map(|e| (from_tagged(&e[0]), from_tagged(&e[1])))
                .collect(),
        ),
        _ => panic!("tag {k}"),
    }
}

fn path(name: &str) -> Input {
    // projects/p/databases/(default)/documents/... -> /databases/(default)/documents/...
    Input::Path(name.split('/').skip(2).map(String::from).collect())
}

/// A Firestore REST value, as the oracle wrote the document.
fn from_rest(j: &Json) -> Input {
    let (k, v) = j.as_object().unwrap().iter().next().unwrap();
    match k.as_str() {
        "integerValue" => Input::Int(v.as_str().unwrap().parse().unwrap()),
        "doubleValue" => Input::Float(v.as_f64().unwrap()),
        "stringValue" => Input::Str(v.as_str().unwrap().into()),
        "booleanValue" => Input::Bool(v.as_bool().unwrap()),
        "nullValue" => Input::Null,
        "bytesValue" => Input::Bytes(unbase64(v.as_str().unwrap())),
        "arrayValue" => Input::List(
            v["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(from_rest)
                .collect(),
        ),
        "mapValue" => Input::Map(fields(&v["fields"])),
        "referenceValue" => path(v.as_str().unwrap()),
        "geoPointValue" => Input::LatLng(
            v["latitude"].as_f64().unwrap(),
            v["longitude"].as_f64().unwrap(),
        ),
        "timestampValue" => {
            // 2020-01-02T03:04:05.123Z
            let s = v.as_str().unwrap();
            let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().unwrap();
            let days = days_from_civil(n(0..4), n(5..7), n(8..10));
            let frac = &s[20..s.len() - 1];
            let nanos = format!("{frac:0<9}").parse().unwrap();
            Input::Ts(
                days * 86_400 + n(11..13) * 3600 + n(14..16) * 60 + n(17..19),
                nanos,
            )
        }
        _ => panic!("REST value {k}"),
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn fields(j: &Json) -> Vec<(Input, Input)> {
    j.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (Input::Str(k.clone()), from_rest(v)))
        .collect()
}

/// The variables a condition sees in the oracle's rules: `resource`, `request`, and the
/// wildcards `database` and `room`.
fn rules_vars(document: &Json) -> Vec<(String, Input)> {
    let name = path("projects/rules/databases/(default)/documents/rooms/r1");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let s = |x: &str| Input::Str(x.into());
    vec![
        (
            "resource".into(),
            Input::Map(vec![
                (s("data"), Input::Map(fields(document))),
                (s("id"), s("r1")),
                (s("__name__"), name.clone()),
            ]),
        ),
        (
            "request".into(),
            Input::Map(vec![
                (s("auth"), Input::Null),
                (s("method"), s("get")),
                (s("path"), name),
                (
                    s("time"),
                    Input::Ts(now.as_secs() as i64, now.subsec_nanos()),
                ),
            ]),
        ),
        ("database".into(), s("(default)")),
        ("room".into(), s("r1")),
    ]
}

fn same(got: &Json, want: &Json) -> bool {
    match (
        got.as_object().unwrap().iter().next().unwrap(),
        want.as_object().unwrap().iter().next().unwrap(),
    ) {
        ((k1, v1), (k2, v2)) if k1 == k2 && k1 == "list" => {
            let (a, b) = (v1.as_array().unwrap(), v2.as_array().unwrap());
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
        }
        ((k1, v1), (k2, v2)) if k1 == k2 && k1 == "map" => {
            let (a, b) = (v1.as_array().unwrap(), v2.as_array().unwrap());
            a.len() == b.len()
                && b.iter()
                    .all(|e| a.iter().any(|g| same(&g[0], &e[0]) && same(&g[1], &e[1])))
        }
        ((k1, v1), (k2, v2)) if k1 == k2 && k1 == "double" => match (v1.as_f64(), v2.as_f64()) {
            (Some(x), Some(y)) => x == y && x.is_sign_negative() == y.is_sign_negative(),
            _ => v1 == v2,
        },
        ((k1, v1), (k2, v2)) => k1 == k2 && v1 == v2,
    }
}

fn outcome(r: &Result<Json, Failure>) -> &'static str {
    match r {
        Ok(v) if v == &json!({"bool": true}) => "allow",
        Ok(v) if v == &json!({"bool": false}) => "false",
        Ok(_) | Err(Failure::Eval(_)) => "error",
        Err(Failure::Compile(_)) => "compile",
    }
}

fn load(rel: &str) -> Json {
    let p = format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}")))
        .unwrap()
}

#[derive(Default)]
struct Tally {
    total: usize,
    pass: [usize; 4],
}

fn main() {
    let show = std::env::args().nth(2);
    let show = show.as_deref().and_then(|s| {
        EVALUATORS
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(s))
    });
    let cel = load("cases/cel-spec.json");
    let rules = load("cases/rules.json");
    let heldout = load("cases/rules-heldout.json");
    let oracle = load("../../results/rules-conditions-official-v1.22.0.json");

    // CEL view: each cel-spec expression against the value cel-spec expects.
    let mut by_file: BTreeMap<String, Tally> = BTreeMap::new();
    for case in cel["cases"].as_array().unwrap() {
        let vars: Vec<(String, Input)> = case["bindings"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), from_tagged(v)))
            .collect();
        let want = &case["expect"];
        let t = by_file
            .entry(case["file"].as_str().unwrap().into())
            .or_default();
        t.total += 1;
        for (i, (name, eval)) in EVALUATORS.iter().enumerate() {
            let got = eval(case["expr"].as_str().unwrap(), &vars);
            let ok = match (&got, want.get("error")) {
                (Err(_), Some(_)) => true,
                (Ok(v), None) => same(v, want),
                _ => false,
            };
            if ok {
                t.pass[i] += 1;
            } else if show == Some(i) {
                println!(
                    "cel {name} {}/{}/{}: {:?} want {want} got {got:?}",
                    case["file"], case["section"], case["name"], case["expr"]
                );
            }
        }
    }

    // Rules view: each condition the official emulator decided, against its outcome. A Rules
    // case is evaluated as written. A cel-spec case's condition checks that the expression has
    // the value cel-spec expects (conditions.py); its outcome here comes from evaluating the
    // expression and comparing the value the same way, so cel-rust is not failed for the `is`
    // in the check.
    let groups: Vec<String> = rules
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["group"].as_str().unwrap().to_string())
        .collect();
    let cel_cases: BTreeMap<String, &Json> = cel["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                format!(
                    "cel/{}/{}/{}",
                    c["file"].as_str().unwrap(),
                    c["section"].as_str().unwrap(),
                    c["name"].as_str().unwrap()
                ),
                c,
            )
        })
        .collect();
    let vars = rules_vars(&oracle["document"]);
    let mut by_group: BTreeMap<String, Tally> = BTreeMap::new();
    let mut internal = 0;
    for r in oracle["results"].as_array().unwrap() {
        let name = r["name"].as_str().unwrap();
        let (group, case) = if let Some(i) = name.strip_prefix("rules/") {
            (
                format!("rules: {}", groups[i.parse::<usize>().unwrap()]),
                None,
            )
        } else if let Some(i) = name.strip_prefix("heldout/") {
            (
                format!(
                    "held out: {}",
                    heldout[i.parse::<usize>().unwrap()]["group"]
                        .as_str()
                        .unwrap()
                ),
                None,
            )
        } else if let Some(c) = name.starts_with("cel/").then(|| cel_cases[name]) {
            (
                format!("cel-spec: {}", c["file"].as_str().unwrap()),
                Some(c),
            )
        } else {
            continue;
        };
        let want = r["outcome"].as_str().unwrap();
        if want == "internal" {
            internal += 1;
            continue;
        }
        let t = by_group.entry(group).or_default();
        t.total += 1;
        for (i, (ev, eval)) in EVALUATORS.iter().enumerate() {
            let src = case.map_or(r["condition"].as_str().unwrap(), |c| {
                c["expr"].as_str().unwrap()
            });
            let res = eval(src, &vars);
            let got = match (case, &res) {
                (None, res) => outcome(res),
                (Some(_), Err(Failure::Compile(_))) => "compile",
                (Some(_), Err(Failure::Eval(_))) => "error",
                (Some(c), Ok(_)) if c["expect"].get("error").is_some() => "allow",
                (Some(c), Ok(v)) if same(v, &c["expect"]) => "allow",
                (Some(_), Ok(_)) => "false",
            };
            if got == want {
                t.pass[i] += 1;
            } else if show == Some(i) {
                println!("rules {ev} {name}: {src:?} want {want} got {got}: {res:?}");
            }
        }
    }
    if show.is_some() {
        return;
    }

    let header = format!(
        "| Group | Cases | {} |",
        EVALUATORS.map(|(n, _)| n).join(" | ")
    );
    let rule = format!("|---|---:|{}", "---:|".repeat(EVALUATORS.len()));
    let row = |name: &str, t: &Tally| {
        println!(
            "| {name} | {} | {} |",
            t.total,
            t.pass.map(|p| p.to_string()).join(" | ")
        );
    };
    let total = |m: &BTreeMap<String, Tally>| {
        let mut s = Tally::default();
        for t in m.values() {
            s.total += t.total;
            for i in 0..4 {
                s.pass[i] += t.pass[i];
            }
        }
        s
    };
    println!(
        "CEL view: cel-spec expressions against cel-spec's expected values\n\n{header}\n{rule}"
    );
    for (k, t) in &by_file {
        row(k, t);
    }
    row("**total**", &total(&by_file));
    println!(
        "\nRules view: conditions against the official emulator's outcome ({internal} internal errors left out)\n\n{header}\n{rule}"
    );
    for (k, t) in &by_group {
        row(k, t);
    }
    row("**total**", &total(&by_group));
}
