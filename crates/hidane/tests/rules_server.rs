//! Security Rules end to end, against the official emulator's recordings: the cases of
//! `tools/oracle/rules_{request,writes,lookups,list,auth,denials,expressions}.py`
//! (`fixtures/rules_*.json`), each a list of REST calls, are replayed in order against a fresh
//! hidane. Every answer's status must match, and a denial must say what the official emulator's
//! does, as `common::rules::same_denial` compares them. A transaction the official emulator began
//! is named by hidane's ID for it in the steps after.

mod common;

use std::{collections::HashMap, net::SocketAddr};

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn start() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    // `debug()` prints to standard output; `rules_coverage.rs` compares what it prints.
    admin
        .rules()
        .set_debug_log(std::sync::Arc::new(|_: &str| {}));
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    addr
}

async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
    auth: Option<&str>,
) -> (u16, Json) {
    let body = body.unwrap_or_default();
    let auth = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{auth}\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
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
    let json = serde_json::from_slice(&content)
        .unwrap_or_else(|_| Json::String(String::from_utf8_lossy(&content).into_owned()));
    (head[9..12].parse().unwrap(), json)
}

fn message(response: &Json) -> Option<&str> {
    response.pointer("/error/message").and_then(Json::as_str)
}

/// Whether hidane's error message says what the official emulator's does: denials as
/// [`common::rules::same_denial`] decides; other errors only by status.
fn same_message(hidane: Option<&str>, official: Option<&str>, status: u16) -> bool {
    match (hidane, official) {
        (Some(hidane), Some(official)) if status == 403 => {
            common::rules::same_denial(hidane, official)
        }
        _ => true,
    }
}

/// The ID of the transaction an answer began: `transaction`, at its top or in its first item.
fn transaction(answer: &Json) -> Option<&str> {
    answer
        .get("transaction")
        .or_else(|| answer.get(0)?.get("transaction"))
        .and_then(Json::as_str)
}

/// `body` with each transaction the official emulator began named by hidane's ID for it.
fn with_transactions(body: &Json, transactions: &HashMap<String, String>) -> Json {
    match body {
        Json::String(s) => transactions
            .get(s)
            .map_or_else(|| body.clone(), |ours| Json::String(ours.clone())),
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| with_transactions(item, transactions))
                .collect(),
        ),
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), with_transactions(v, transactions)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Replays every case of `fixture` but those `skip` names, each case's steps in order on one
/// server. A case stops at its first answer that differs; the differences are listed at the end.
async fn replay(fixture: &str, skip: &[&str]) {
    let cases: Vec<Json> = serde_json::from_str(fixture).unwrap();
    let addr = start().await;
    let mut compared = 0;
    let mut differences = Vec::new();
    // hidane's ID for each transaction the official emulator began.
    let mut transactions: HashMap<String, String> = HashMap::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        if skip.contains(&name) {
            continue;
        }
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let body = (!step["body"].is_null())
                .then(|| with_transactions(&step["body"], &transactions).to_string());
            let (status, response) = send(
                addr,
                step["method"].as_str().unwrap(),
                step["path"].as_str().unwrap(),
                body.as_deref(),
                step["auth"].as_str(),
            )
            .await;
            if let (Some(official), Some(ours)) =
                (transaction(&step["response"]), transaction(&response))
            {
                transactions.insert(official.to_owned(), ours.to_owned());
            }
            let same = status == step["status"]
                && same_message(message(&response), message(&step["response"]), status);
            if !same {
                differences.push(format!(
                    "{name}, step {i}: {} {}\n  official: {} {}\n  hidane:   {status} {}",
                    step["method"], step["path"], step["status"], step["response"], response
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
async fn request_and_resource() {
    replay(include_str!("fixtures/rules_request.json"), &[]).await;
}

/// The cases of `rules_writes.json` where hidane's denial differs on purpose (D019).
const ALLOWED_THEN_UNMATCHED: &[&str] = &[
    "batchGet, allowed, then no statement",
    "commit, allowed, then no statement",
];

#[tokio::test]
async fn writes_are_checked_as_create_update_or_delete() {
    replay(
        include_str!("fixtures/rules_writes.json"),
        ALLOWED_THEN_UNMATCHED,
    )
    .await;
}

/// A commit that writes one document more than once is checked once for it, as the official
/// emulator collapses the writes (`tools/oracle/rules_same_document.py`).
#[tokio::test]
async fn a_document_written_more_than_once_is_checked_once() {
    replay(include_str!("fixtures/rules_same_document.json"), &[]).await;
}

/// docs/parity-exceptions.md D019: a batch is denied on a document or a write no statement
/// matches, after one that was allowed. The official emulator lists the allowed one's
/// statements as `false`; hidane answers what the denied one alone answers.
#[tokio::test]
async fn a_batch_denied_where_no_statement_matches() {
    let cases: Vec<Json> =
        serde_json::from_str(include_str!("fixtures/rules_writes.json")).unwrap();
    let addr = start().await;
    for name in ALLOWED_THEN_UNMATCHED {
        let case = cases.iter().find(|c| c["name"] == *name).unwrap();
        let steps = case["steps"].as_array().unwrap();
        let mut answer = None;
        for step in steps {
            let body = (!step["body"].is_null()).then(|| step["body"].to_string());
            answer = Some(
                send(
                    addr,
                    step["method"].as_str().unwrap(),
                    step["path"].as_str().unwrap(),
                    body.as_deref(),
                    step["auth"].as_str(),
                )
                .await,
            );
        }
        let (status, response) = answer.unwrap();
        let official = &steps.last().unwrap()["response"];
        assert_eq!(status, 403, "{name}");
        assert!(
            message(official).unwrap().starts_with("\nfalse for '"),
            "{name}: {official}"
        );
        assert_eq!(
            message(&response),
            Some("No matching allow statements"),
            "{name}"
        );
    }
}

#[tokio::test]
async fn lookups_and_their_limits() {
    replay(include_str!("fixtures/rules_lookups.json"), &[]).await;
}

#[tokio::test]
async fn lists_are_decided_by_their_filters() {
    replay(include_str!("fixtures/rules_list.json"), &[]).await;
}

#[tokio::test]
async fn request_auth_from_tokens() {
    replay(include_str!("fixtures/rules_auth.json"), &[]).await;
}

#[tokio::test]
async fn denials() {
    replay(include_str!("fixtures/rules_denials.json"), &[]).await;
}

/// The batches of `rules_eval.py` (`hidane-rules/tests/fixtures/rules_eval.json`) as commits over
/// REST, which tell which documents cost a lookup. The two the official emulator allows in some
/// projects and runs and denies in others (`answers`, #210) are allowed: every document a commit
/// writes is free for its writes' `get()` and `exists()` (D020 in `docs/parity-exceptions.md`).
#[tokio::test]
async fn documents_a_batch_writes_are_free_to_look_up() {
    let fixture: Json = serde_json::from_str(include_str!(
        "../../hidane-rules/tests/fixtures/rules_eval.json"
    ))
    .unwrap();
    let addr = start().await;
    let base = "projects/rules-eval/databases/(default)/documents";
    let owner = Some("Bearer owner");
    let mut compared = 0;
    for result in fixture["results"].as_array().unwrap() {
        let name = result["name"].as_str().unwrap();
        if !name.starts_with("batches/") {
            continue;
        }
        let content = format!(
            "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n    match /c/{{d}} {{\n      allow read: if true;\n      allow write: if {};\n    }}\n  }}\n}}\n",
            result["condition"].as_str().unwrap()
        );
        let rules = serde_json::json!({"rules": {"files": [{"name": "firestore.rules", "content": content}]}});
        let path = "/emulator/v1/projects/rules-eval:securityRules";
        let (status, _) = send(addr, "PUT", path, Some(&rules.to_string()), None).await;
        assert_eq!(status, 200, "{name}");
        for d in ["u", "v", "w"] {
            send(addr, "DELETE", &format!("/v1/{base}/c/{d}"), None, owner).await;
        }
        let stored = r#"{"fields": {"n": {"integerValue": "1"}}}"#;
        let (status, _) = send(
            addr,
            "PATCH",
            &format!("/v1/{base}/c/x"),
            Some(stored),
            owner,
        )
        .await;
        assert_eq!(status, 200, "{name}");
        let writes: Vec<Json> = result["writes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                let w = w.as_str().unwrap();
                let doc = format!("{base}/c/{}", &w[1..]);
                if w.starts_with('-') {
                    serde_json::json!({"delete": doc})
                } else {
                    serde_json::json!({"update": {"name": doc, "fields": {"n": {"integerValue": "2"}}}})
                }
            })
            .collect();
        let commit = serde_json::json!({ "writes": writes }).to_string();
        let (status, response) = send(
            addr,
            "POST",
            &format!("/v1/{base}:commit"),
            Some(&commit),
            None,
        )
        .await;
        let allowed = result.get("answers").is_some() || result["outcome"] == "allow";
        assert_eq!(
            status,
            if allowed { 200 } else { 403 },
            "{name}: {response}"
        );
        compared += 1;
    }
    assert_eq!(compared, 7);
}

#[tokio::test]
async fn expressions_and_their_limit() {
    replay(include_str!("fixtures/rules_expressions.json"), &[]).await;
}
