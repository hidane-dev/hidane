//! `update_time` preconditions against the official emulator's recordings
//! (`fixtures/base_versions.json`, from `tools/oracle/base_versions.py`, #262): the version each
//! kind of write may name, the times a precondition may not hold, the Datastore key printed for
//! a version past the stored one, writes that repeat a precondition, and the `allow` method
//! Security Rules check. Times in the recordings are templates, filled with hidane's own commit
//! times.

mod common;

use std::net::SocketAddr;

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};
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

async fn send(addr: SocketAddr, method: &str, path: &str, body: &Json, owner: bool) -> (u16, Json) {
    let body = body.to_string();
    let auth = if owner {
        "Authorization: Bearer owner\r\n"
    } else {
        ""
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    (
        head[9..12].parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}

/// Replaces each `{<letter><k>[±shift]}` in `text` with `fill(commit k, shift)`.
fn fill(text: &str, letter: char, commits: &[i64], format: impl Fn(i64) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let open = format!("{{{letter}");
    while let Some(at) = rest.find(&open) {
        out.push_str(&rest[..at]);
        let inner = &rest[at + 2..];
        let end = inner.find('}').unwrap();
        let spec = &inner[..end];
        let split = spec.find(['+', '-']).unwrap_or(spec.len());
        let k: usize = spec[..split].parse().unwrap();
        let shift: i64 = if split < spec.len() {
            spec[split..].trim_start_matches('+').parse().unwrap()
        } else {
            0
        };
        out.push_str(&format(commits[k - 1] + shift));
        rest = &inner[end + 1..];
    }
    out.push_str(rest);
    out
}

fn time(micros: i64) -> String {
    DateTime::<Utc>::from_timestamp_micros(micros)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string()
}

/// Cases where hidane answers differently on purpose, with the row of
/// `docs/parity-exceptions.md` that says why: REST's `currentDocument.updateTime` parameter,
/// which the official emulator reads as version 0, and hidane as the time it names.
const INTENTIONAL: &[(&str, &str)] = &[(
    "REST patch, currentDocument.updateTime at the stored version",
    "D066",
)];

fn micros(text: &str) -> i64 {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp_micros()
}

#[tokio::test]
async fn update_time_preconditions_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/base_versions.json")).unwrap();
    let addr = start().await;
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let project = case["steps"][0]["path"]
            .as_str()
            .unwrap()
            .split('/')
            .nth(3)
            .unwrap();
        if case["rules"].as_bool().unwrap() {
            let rules = if name.starts_with("rules that deny") {
                "allow read, write: if false;"
            } else {
                "allow create: if request.resource.data.k == 'c';\n      allow update: if request.resource.data.k == 'u';\n      allow read: if true;"
            };
            let content = format!(
                "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n    match /c/{{d}} {{\n      {rules}\n    }}\n  }}\n}}\n"
            );
            let body =
                json!({"rules": {"files": [{"name": "firestore.rules", "content": content}]}});
            let (status, _) = send(
                addr,
                "PUT",
                &format!("/emulator/v1/projects/{project}:securityRules"),
                &body,
                true,
            )
            .await;
            assert_eq!(status, 200, "{name}: rules");
        }
        let mut commits = Vec::new();
        for step in case["steps"].as_array().unwrap() {
            let body: Json =
                serde_json::from_str(&fill(&step["body"].to_string(), 'C', &commits, time))
                    .unwrap();
            let path = fill(step["path"].as_str().unwrap(), 'C', &commits, time);
            let method = step["method"].as_str().unwrap();
            let (status, answer) =
                send(addr, method, &path, &body, step["owner"].as_bool().unwrap()).await;
            if INTENTIONAL.iter().any(|(case, _)| name == *case) && method == "PATCH" {
                // The official emulator refuses the stored version as not version 0; hidane
                // takes it.
                assert_eq!(step["error"]["status"], "FAILED_PRECONDITION", "{name}");
                assert_eq!(status, 200, "{name}: {answer}");
                continue;
            }
            let official = &step["error"];
            let same = if status == 200 {
                if let Some(time) = answer["commitTime"].as_str() {
                    commits.push(micros(time));
                }
                step["status"] == 200
            } else {
                let error = &answer["error"];
                let want = fill(
                    official["message"].as_str().unwrap_or_default(),
                    'V',
                    &commits,
                    |v| v.to_string(),
                );
                step["status"] == status
                    && error["status"] == official["status"]
                    // The wording of a missing document is production Firestore's
                    // (docs/parity-exceptions.md).
                    && (error["status"] == "NOT_FOUND" || error["message"] == want.as_str())
            };
            if !same {
                differences.push(format!(
                    "{name}:\n    official {} {official}\n    hidane   {status} {answer}",
                    step["status"]
                ));
                break;
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
