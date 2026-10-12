//! The Security Rules API and `debug()`, against the official emulator's recordings
//! (`tools/oracle/rules_coverage.py`: `fixtures/rules_coverage.json`, and with `--rules`,
//! `fixtures/rules_coverage_flag.json`). Each case's steps are replayed in order against a fresh
//! hidane, and every answer's status must match. So must:
//!
//! - what `debug()` printed during each step, line for line;
//! - the body of `GET :securityRules` and `GET :ruleCoverage`, byte for byte, but for the values
//!   of the expression `request` (it holds the time, and fields hidane does not have), which are
//!   left out of both; and the data the page of `GET :ruleCoverage.html` shows.
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

/// A report's expressions `request` without their values, as text.
fn without_request(body: &str) -> String {
    let mut json: Json = serde_json::from_str(body).unwrap();
    let content: Vec<char> = json["rules"]["files"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .collect();
    fn walk(nodes: &mut Json, content: &[char]) {
        for node in nodes.as_array_mut().into_iter().flatten() {
            let position = &node["sourcePosition"];
            let at = |key: &str| position[key].as_u64().unwrap_or(0) as usize;
            let text: String = content
                .get(at("currentOffset")..=at("endOffset"))
                .unwrap_or_default()
                .iter()
                .collect();
            if text == "request"
                && let Some(fields) = node.as_object_mut()
            {
                fields.remove("values");
            }
            walk(&mut node["children"], content);
        }
    }
    walk(&mut json["report"], &content);
    serde_json::to_string_pretty(&json).unwrap()
}

/// Whether hidane's body says what the official emulator's does (see the module's comment).
fn same_body(path: &str, hidane: &str, official: &str) -> bool {
    if path.contains(":ruleCoverage.html") {
        return page_data(hidane) == page_data(official);
    }
    hidane == official
        || (path.contains(":ruleCoverage") && without_request(hidane) == without_request(official))
}

async fn replay(fixture: &str, with_rules_file: bool) {
    let cases: Vec<Json> = serde_json::from_str(fixture).unwrap();
    let (addr, printed) = start(with_rules_file).await;
    let mut differences = Vec::new();
    let mut compared = 0;
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let differs = case.get("differs").is_some();
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let (status, content_type, body) = send(addr, step).await;
            let log = std::mem::take(&mut *printed.lock().unwrap());
            let path = step["path"].as_str().unwrap();
            let mut problems = Vec::new();
            if status != step["status"] {
                problems.push(format!(
                    "status {status}, official {}: {body}",
                    step["status"]
                ));
            }
            if !differs && log != step["log"].as_str().unwrap_or_default() {
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
