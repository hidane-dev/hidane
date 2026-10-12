//! `PUT :securityRules` against the official emulator's recordings (`fixtures/rules_compile.json`,
//! from `tools/oracle/rules_compile.py`): every case is sent to a fresh hidane and the status
//! and the body are compared byte for byte with the recorded answer printed in the official
//! layout (protobuf-java's JsonFormat for `issues[]`, one line for errors).

use std::net::SocketAddr;

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// Cases whose first error line matches but not the rest: the lines after the first come from
/// ANTLR's error recovery, which hidane follows only where it is simple
/// (`docs/parity-exceptions.md`).
const FIRST_LINE_ONLY: &[&str] = &[];

/// The official emulator answers these with a 500 (a crash); hidane with a 400 "Payload isn't
/// valid for request." (`docs/parity-exceptions.md`).
const CRASHES: &[&str] = &["no files", "no rules"];

async fn start() -> (SocketAddr, hidane::Admin) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin.clone()),
        std::future::pending(),
    ));
    (addr, admin)
}

async fn put(addr: SocketAddr, project: &str, body: &str) -> (u16, String) {
    let request = format!(
        "PUT /emulator/v1/projects/{project}:securityRules HTTP/1.1\r\nHost: localhost\r\n\
         Connection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    (head[9..12].parse().unwrap(), body.to_owned())
}

/// The request `rules_compile.py` sent for a case.
fn request(case: &Json) -> String {
    if let Some(raw) = case["raw"].as_str() {
        return raw.to_owned();
    }
    if !case["request"].is_null() {
        return case["request"].to_string();
    }
    let name = case["fileName"].as_str().unwrap_or("firestore.rules");
    serde_json::json!({"rules": {"files": [{"name": name, "content": case["content"]}]}})
        .to_string()
}

/// A recorded answer as the official emulator printed it.
fn official(response: &Json) -> String {
    if let Some(error) = response.get("error") {
        let message = match error.get("message").and_then(Json::as_str) {
            Some(m) => format!("\"message\":{},", escape(m, true)),
            None => String::new(),
        };
        return format!(
            "{{\"error\":{{\"code\":{},{message}\"status\":{}}}}}",
            error["code"], error["status"]
        );
    }
    let mut out = String::new();
    pretty(response, 0, &mut out);
    out.push('\n');
    out
}

/// protobuf-java's JsonFormat layout.
fn pretty(value: &Json, indent: usize, out: &mut String) {
    match value {
        Json::Object(fields) if fields.is_empty() => {
            out.push_str(&format!("{{\n{}}}", " ".repeat(indent)));
        }
        Json::Object(fields) => {
            out.push_str("{\n");
            for (i, (key, value)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&format!("{}\"{key}\": ", " ".repeat(indent + 2)));
                pretty(value, indent + 2, out);
            }
            out.push_str(&format!("\n{}}}", " ".repeat(indent)));
        }
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                pretty(item, indent, out);
            }
            out.push(']');
        }
        Json::String(s) => out.push_str(&escape(s, false)),
        other => out.push_str(&other.to_string()),
    }
}

/// A JSON string as the official emulator escapes it; error messages also escape `/`.
fn escape(s: &str, slash: bool) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' if slash => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn first_line(body: &str) -> Option<String> {
    let json: Json = serde_json::from_str(body).ok()?;
    let message = json["error"]["message"].as_str()?;
    message.lines().nth(1).map(str::to_owned)
}

#[tokio::test]
async fn compiles_like_the_official_emulator() {
    let fixture: Vec<Json> =
        serde_json::from_str(include_str!("fixtures/rules_compile.json")).unwrap();
    let (addr, _) = start().await;
    let mut failures = Vec::new();
    let mut first_line_only = Vec::new();
    for case in &fixture {
        let name = case["name"].as_str().unwrap();
        let (status, body) = put(addr, "compile", &request(case)).await;
        if CRASHES.contains(&name) {
            assert_eq!(status, 400, "{name}");
            assert_eq!(
                body,
                r#"{"error":{"code":400,"message":"Payload isn't valid for request.","status":"INVALID_ARGUMENT"}}"#,
                "{name}"
            );
            continue;
        }
        let expected = official(&case["response"]);
        let expected_status = case["status"].as_u64().unwrap();
        if u64::from(status) == expected_status && body == expected {
            if FIRST_LINE_ONLY.contains(&name) {
                failures.push(format!(
                    "{name}: matches in full, take it off FIRST_LINE_ONLY"
                ));
            }
            continue;
        }
        if FIRST_LINE_ONLY.contains(&name)
            && u64::from(status) == expected_status
            && first_line(&body).is_some()
            && first_line(&body) == first_line(&expected)
        {
            first_line_only.push(name);
            continue;
        }
        failures.push(format!(
            "{name}: got {status}\n{body}\nexpected {expected_status}\n{expected}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert_eq!(first_line_only.len(), FIRST_LINE_ONLY.len());
}

/// The ruleset is kept per database (`:securityRules` of a project is its default database's);
/// one that does not compile leaves the previous one.
#[tokio::test]
async fn keeps_the_last_ruleset_that_compiled() {
    let (addr, admin) = start().await;
    let rules = |content: &str| {
        serde_json::json!({"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
            .to_string()
    };
    let good = "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} {\n      allow read: if true;\n    }\n  }\n}\n";
    let a = "projects/a/databases/(default)";
    assert_eq!(put(addr, "a", &rules(good)).await.0, 200);
    let ruleset = admin.rules().ruleset(a).expect("compiled");
    assert_eq!(ruleset.version, hidane_rules::Version::V2);
    assert_eq!(ruleset.file_name.as_deref(), Some("firestore.rules"));
    assert!(ruleset.service("cloud.firestore").is_some());
    assert!(
        admin
            .rules()
            .ruleset("projects/b/databases/(default)")
            .is_none()
    );
    assert!(
        admin
            .rules()
            .ruleset("projects/a/databases/other")
            .is_none()
    );

    assert_eq!(put(addr, "a", &rules("service x {")).await.0, 400);
    assert!(
        admin
            .rules()
            .ruleset(a)
            .is_some_and(|r| r.version == hidane_rules::Version::V2)
    );
}
