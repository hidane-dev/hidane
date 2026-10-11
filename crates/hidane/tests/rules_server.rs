//! Security Rules end to end, against the official emulator's recordings: the cases of
//! `tools/oracle/rules_{request,writes,lookups,list,auth,denials}.py` (`fixtures/rules_*.json`),
//! each a list of REST calls, are replayed in order against a fresh hidane. Every answer's
//! status must match, and a denial must say what the official emulator's does, as
//! `common::rules::same_denial` compares them.

mod common;

use std::net::SocketAddr;

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn start() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
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

/// Replays every case of `fixture` but those `skip` names, each case's steps in order on one
/// server. A case stops at its first answer that differs; the differences are listed at the end.
async fn replay(fixture: &str, skip: &[&str]) {
    let cases: Vec<Json> = serde_json::from_str(fixture).unwrap();
    let addr = start().await;
    let mut compared = 0;
    let mut differences = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        if skip.contains(&name) {
            continue;
        }
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let body = (!step["body"].is_null()).then(|| step["body"].to_string());
            let (status, response) = send(
                addr,
                step["method"].as_str().unwrap(),
                step["path"].as_str().unwrap(),
                body.as_deref(),
                step["auth"].as_str(),
            )
            .await;
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

#[tokio::test]
async fn writes_are_checked_as_create_update_or_delete() {
    replay(include_str!("fixtures/rules_writes.json"), &[]).await;
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
    // `:ruleCoverage` is not served yet.
    replay(include_str!("fixtures/rules_denials.json"), &["coverage"]).await;
}
