//! The Functions emulator's Firestore trigger registrations against the official emulator's
//! recordings (`fixtures/function_triggers.json`, from `tools/oracle/function_triggers.py`):
//! status, content type and body, case by case, for the runs without and with
//! `--functions_emulator`, each against a fresh emulator. Then what was registered.

use std::net::SocketAddr;

use hidane::triggers::{Document, EventTrigger, EventarcTrigger, MatchType, Trigger};
use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

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

/// Sends the case's request as it was sent to the official emulator; returns the outcome in
/// the fixture's shape.
async fn send(addr: SocketAddr, case: &Json) -> Json {
    let method = case["method"].as_str().unwrap();
    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
        case["path"].as_str().unwrap()
    );
    for (name, value) in case["headers"].as_object().unwrap() {
        request.push_str(&format!("{name}: {}\r\n", value.as_str().unwrap()));
    }
    let body = case["body"].as_str();
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body.unwrap_or_default());
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status: i64 = lines.next().unwrap()[9..12].parse().unwrap();
    let content_type = lines
        .filter_map(|line| line.split_once(": "))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.to_owned());
    serde_json::json!({"status": status, "content_type": content_type, "body": body})
}

#[tokio::test]
async fn registrations_answer_like_the_official_emulator() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/function_triggers.json")).unwrap();
    let runs = fixture["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    let mut differences = Vec::new();
    for run in runs {
        let (addr, _) = start().await;
        for case in run["cases"].as_array().unwrap() {
            let actual = send(addr, case).await;
            if actual != case["outcome"] {
                differences.push(format!(
                    "{} {}:\n    official {}\n    hidane   {actual}",
                    run["flags"],
                    case["name"].as_str().unwrap(),
                    case["outcome"]
                ));
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// The registry #70 delivers from: what was accepted, by project and key, the last registration
/// of a key replacing the one before; nothing for what was refused.
#[tokio::test]
async fn accepted_triggers_are_kept_by_project_and_key() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/function_triggers.json")).unwrap();
    let (addr, admin) = start().await;
    for case in fixture["runs"][0]["cases"].as_array().unwrap() {
        send(addr, case).await;
    }
    let registered = admin.triggers().registered("demo-triggers");
    let get = |key: &str| {
        registered
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, trigger)| trigger.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        get("us-central1-v1created-0"),
        [Trigger::V1(EventTrigger {
            event_type: "providers/cloud.firestore/eventTypes/document.write".into(),
            resource: "projects/demo-triggers/databases/(default)/documents/items/{id}".into(),
            service: "firestore.googleapis.com".into(),
        })]
    );
    assert_eq!(
        get("us-central1-v2created-0"),
        [Trigger::V2(EventarcTrigger {
            event_type: "google.cloud.firestore.document.v1.created".into(),
            database: "(default)".into(),
            namespace: "(default)".into(),
            document: Some(Document {
                value: "items/{id}".into(),
                match_type: MatchType::PATH_PATTERN,
            }),
        })]
    );
    assert_eq!(
        get("db"),
        [Trigger::V2(EventarcTrigger {
            event_type: "google.cloud.firestore.document.v1.created".into(),
            database: "other-db".into(),
            namespace: "(default)".into(),
            document: Some(Document {
                value: "items/{id}".into(),
                match_type: MatchType::PATH_PATTERN,
            }),
        })]
    );
    assert_eq!(admin.triggers().registered("other-project").len(), 1);
    // Refused: a project that is not a project ID.
    assert!(admin.triggers().registered("Demo").is_empty());
    // Whitespace around a project is not part of it.
    assert_eq!(admin.triggers().registered("demo").len(), 1);
}
