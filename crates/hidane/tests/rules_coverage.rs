//! The Security Rules API and `debug()`, against the official emulator's recordings
//! (`tools/oracle/rules_coverage.py`: `fixtures/rules_coverage.json`, and with `--rules`,
//! `fixtures/rules_coverage_flag.json`; `tools/oracle/rules_request_fields.py`:
//! `fixtures/rules_request_fields.json`). Each case's steps are replayed in order against a
//! fresh hidane, and every answer's status must match. So must:
//!
//! - what `debug()` printed during each step, line for line;
//! - the body of `GET :securityRules` and `GET :ruleCoverage`, byte for byte, and the data the
//!   page of `GET :ruleCoverage.html` shows.
//!
//! Both leave out the time each request was made, `request.time`: `debug()`'s lines for it,
//! and in a coverage report its value in each value of `request`, whose values that then
//! become equal count as one.
//!
//! A case marked `differs` is one where hidane intentionally does not do what the official
//! emulator does (`docs/parity-exceptions.md`): only its statuses are compared.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// The `--rules` file of `fixtures/rules_coverage_flag.json`, as the jar was given it.
const RULES_FILE: &str = "rules_coverage.rules";

/// A server, and what `debug()` printed on it.
async fn start(with_rules_file: bool) -> (SocketAddr, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    let printed = Arc::new(Mutex::new(String::new()));
    let log = Arc::clone(&printed);
    admin
        .rules()
        .set_debug_log(Arc::new(move |text| log.lock().unwrap().push_str(text)));
    if with_rules_file {
        let content = include_str!("../../../tools/oracle/rules_coverage.rules");
        let ruleset = hidane_rules::compile(&[hidane_rules::SourceFile {
            name: Some(RULES_FILE.to_owned()),
            content: content.to_owned(),
        }])
        .ruleset
        .expect("compiles");
        admin.rules().set_default(
            std::path::Path::new(RULES_FILE),
            content.to_owned(),
            ruleset,
        );
    }
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    (addr, printed)
}

/// The status, content type and body of the answer to one request.
async fn send(addr: SocketAddr, step: &Json) -> (u16, Option<String>, String) {
    let body = if step["body"].is_null() {
        String::new()
    } else {
        step["body"].to_string()
    };
    let mut headers = String::new();
    if let Some(auth) = step["auth"].as_str() {
        headers.push_str(&format!("Authorization: {auth}\r\n"));
    }
    match step.get("content_type") {
        None => headers.push_str("Content-Type: application/json\r\n"),
        Some(Json::String(content_type)) => {
            headers.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        Some(_) => {}
    }
    if !body.is_empty() || step.get("content_type").is_none() {
        headers.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{headers}\r\n{body}",
        step["method"].as_str().unwrap(),
        step["path"].as_str().unwrap(),
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut rest = &raw[split + 4..];
    let mut content = Vec::new();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        loop {
            let line = rest.windows(2).position(|w| w == b"\r\n").unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&rest[..line]).unwrap().trim(), 16)
                    .unwrap();
            if size == 0 {
                break;
            }
            content.extend_from_slice(&rest[line + 2..line + 2 + size]);
            rest = &rest[line + 2 + size + 2..];
        }
    } else {
        content.extend_from_slice(rest);
    }
    let content_type = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-type")
            .then(|| value.trim().to_owned())
    });
    (
        head[9..12].parse().unwrap(),
        content_type,
        String::from_utf8(content).unwrap(),
    )
}

/// The data of a coverage page: `const data = …;`.
fn page_data(html: &str) -> &str {
    let start = html.find("const data = ").expect("data") + "const data = ".len();
    let end = start + html[start..].find("};").expect("end of data") + 1;
    &html[start..end]
}

/// A report without the time of each value of `request` (a map with `time` and `method`);
/// the values of an expression that then are equal count as one.
fn without_time(body: &str) -> String {
    fn walk(nodes: &mut Json) {
        for node in nodes.as_array_mut().into_iter().flatten() {
            if let Some(values) = node["values"].as_array_mut() {
                let mut merged: Vec<Json> = Vec::new();
                for mut entry in values.drain(..) {
                    if let Some(fields) = entry["value"]["mapValue"]["fields"].as_object_mut()
                        && fields.contains_key("method")
                    {
                        fields.remove("time");
                    }
                    match merged.iter_mut().find(|e| e["value"] == entry["value"]) {
                        Some(seen) => {
                            let count = |e: &Json| e["count"].as_u64().unwrap_or(1);
                            seen["count"] = Json::from(count(seen) + count(&entry));
                        }
                        None => merged.push(entry),
                    }
                }
                *values = merged;
            }
            walk(&mut node["children"]);
        }
    }
    let mut json: Json = serde_json::from_str(body).unwrap();
    walk(&mut json["report"]);
    serde_json::to_string_pretty(&json).unwrap()
}

/// What `debug()` printed, without the lines of `request.time`: a map entry with the key
/// `time` loses the seconds and nanoseconds of its timestamp.
fn without_time_lines(log: &str) -> String {
    let mut out = String::new();
    // The indentation of the `key: "time"` line, while in its entry.
    let mut entry: Option<usize> = None;
    for line in log.split_inclusive('\n') {
        let text = line.trim_start();
        let indent = line.len() - text.len();
        match entry {
            Some(at) if indent < at => entry = None,
            Some(_) if text.starts_with("seconds: ") || text.starts_with("nanos: ") => continue,
            _ => {}
        }
        if text.starts_with("key: \"time\"") {
            entry = Some(indent);
        }
        out.push_str(line);
    }
    out
}

/// `step` with hidane's ID for each transaction the official emulator began in its body.
fn with_transactions(step: &Json, transactions: &[(String, String)]) -> Json {
    let mut step = step.clone();
    if !transactions.is_empty() && !step["body"].is_null() {
        let mut body = step["body"].to_string();
        for (official, ours) in transactions {
            body = body.replace(official.as_str(), ours);
        }
        step["body"] = serde_json::from_str(&body).unwrap();
    }
    step
}

/// Whether hidane's body says what the official emulator's does (see the module's comment).
fn same_body(path: &str, hidane: &str, official: &str) -> bool {
    if path.contains(":ruleCoverage.html") {
        return page_data(hidane) == page_data(official);
    }
    hidane == official
        || (path.contains(":ruleCoverage") && without_time(hidane) == without_time(official))
}

async fn replay(fixture: &str, with_rules_file: bool) {
    let cases: Vec<Json> = serde_json::from_str(fixture).unwrap();
    let (addr, printed) = start(with_rules_file).await;
    let mut differences = Vec::new();
    let mut compared = 0;
    // The ID of each transaction the official emulator began, and hidane's.
    let mut transactions: Vec<(String, String)> = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let differs = case.get("differs").is_some();
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let step = &with_transactions(step, &transactions);
            let (status, content_type, body) = send(addr, step).await;
            if let Some(official) = step["response"]["transaction"].as_str()
                && let Ok(answer) = serde_json::from_str::<Json>(&body)
                && let Some(ours) = answer["transaction"].as_str()
            {
                transactions.push((official.to_owned(), ours.to_owned()));
            }
            let log = std::mem::take(&mut *printed.lock().unwrap());
            let path = step["path"].as_str().unwrap();
            let mut problems = Vec::new();
            if status != step["status"] {
                problems.push(format!(
                    "status {status}, official {}: {body}",
                    step["status"]
                ));
            }
            let official_log = step["log"].as_str().unwrap_or_default();
            if !differs && without_time_lines(&log) != without_time_lines(official_log) {
                problems.push(format!(
                    "printed {log:?}, official {:?}",
                    step["log"].as_str().unwrap_or_default()
                ));
            }
            if let Some(raw) = step["raw"].as_str().filter(|_| !differs && status == 200) {
                if !same_body(path, &body, raw) {
                    problems.push(format!("answered\n{body}\nofficial\n{raw}"));
                }
                if content_type.as_deref() != step["answered"].as_str() {
                    problems.push(format!(
                        "content type {content_type:?}, official {}",
                        step["answered"]
                    ));
                }
            }
            if !problems.is_empty() {
                differences.push(format!(
                    "{name}, step {i}: {} {path}\n  {}",
                    step["method"],
                    problems.join("\n  ")
                ));
                break;
            }
            compared += 1;
        }
    }
    assert!(compared > 0);
    assert!(
        differences.is_empty(),
        "{} of {} cases differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}

#[tokio::test]
async fn rules_coverage_and_debug() {
    replay(include_str!("fixtures/rules_coverage.json"), false).await;
}

#[tokio::test]
async fn the_rules_file() {
    replay(include_str!("fixtures/rules_coverage_flag.json"), true).await;
}

#[tokio::test]
async fn request_fields() {
    replay(include_str!("fixtures/rules_request_fields.json"), false).await;
}
