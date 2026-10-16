//! Datastore mode (#81) against the official emulator's recordings: each request of
//! `fixtures/datastore_entities.json` (from `tools/oracle/datastore_entities.py`), in order,
//! to a fresh hidane started with `--database-mode datastore-mode`, its answer compared in full
//! once server times, versions, transaction IDs and cursors are masked as the recorder masks
//! them.

use std::net::SocketAddr;

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// The recorder's project, `datastore-HHMMSS`, as the fixture names it.
const PROJECT: &str = "datastore-000000";

/// What hidane answers otherwise, on purpose (docs/parity-exceptions.md): a well-formed
/// transaction ID that was never issued (D025).
const KNOWN: &[&str] = &[
    "in an unknown transaction",
    "begin a read-write transaction retrying an unknown one",
    "commit an unknown transaction",
];

/// The queries hidane answers otherwise, on purpose (docs/parity-exceptions.md).
const KNOWN_QUERIES: &[&str] = &[];

async fn start() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default().with_datastore_mode(true);
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    addr
}

struct Answer {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
    json: bool,
) -> Answer {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if json {
        request.push_str("Content-Type: application/json\r\n");
    }
    let body = body.unwrap_or_default();
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines.next().unwrap()[9..12].parse().unwrap();
    let mut content_type = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line.split_once(": ").unwrap();
        match name.to_ascii_lowercase().as_str() {
            "content-type" => content_type = Some(value.to_owned()),
            "transfer-encoding" => chunked = value == "chunked",
            _ => {}
        }
    }
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_owned()
    };
    Answer {
        status,
        content_type,
        body,
    }
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    loop {
        let (size, rest) = body.split_once("\r\n").unwrap();
        let size = usize::from_str_radix(size.trim(), 16).unwrap();
        if size == 0 {
            return out;
        }
        out.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
}

/// Replaces the string value of every `"key": "…"` with `mask`.
fn mask_values(text: &str, key: &str, mask: &str) -> String {
    let marker = format!("\"{key}\": \"");
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(&marker) {
        out.push_str(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];
        let end = rest.find('"').unwrap();
        out.push_str(mask);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The recorder's masking (`tools/oracle/datastore_entities.py`).
fn normalize(text: &str) -> String {
    let mut text = text.replace(PROJECT, "{project}");
    for key in ["createTime", "updateTime", "readTime", "commitTime"] {
        text = mask_values(&text, key, "<time>");
    }
    for key in ["version", "commitVersion", "snapshotVersion"] {
        text = mask_values(&text, key, "<version>");
    }
    for key in ["cursor", "endCursor", "skippedCursor", "startCursor"] {
        text = mask_values(&text, key, "<cursor>");
    }
    mask_values(&text, "transaction", "<id>")
}

/// The value at `path` (dotted, list indexes as numbers) of `json`.
fn at<'a>(json: &'a Json, path: &str) -> Option<&'a Json> {
    path.split('.')
        .try_fold(json, |value, part| match part.parse::<usize>() {
            Ok(i) => value.get(i),
            Err(_) => value.get(part),
        })
}

/// Sends a fixture's requests in order to a fresh hidane, after writing its dataset; the
/// requests that answer otherwise, but those `known` lists.
async fn replay(fixture: &Json, known: &[&str]) -> Vec<String> {
    assert_eq!(fixture["mode"], "datastore-mode");
    let addr = start().await;
    if let Some(dataset) = fixture.get("dataset").and_then(Json::as_array) {
        for chunk in dataset.chunks(500) {
            let mutations: Vec<Json> = chunk
                .iter()
                .map(|e| serde_json::json!({"upsert": {"key": e["key"], "properties": e["properties"]}}))
                .collect();
            let body = serde_json::json!({"mode": "NON_TRANSACTIONAL", "mutations": mutations});
            let answer = send(
                addr,
                "POST",
                &format!("/v1/projects/{PROJECT}:commit"),
                Some(&body.to_string()),
                true,
            )
            .await;
            assert_eq!(answer.status, 200, "{}", answer.body);
        }
    }
    // Transactions and cursors the server gave, by the names the fixture gives them.
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let path = case["path"].as_str().unwrap().replace("{project}", PROJECT);
        let mut body = match (case.get("raw"), case.get("body")) {
            (Some(raw), _) => Some(raw.as_str().unwrap().replace("{project}", PROJECT)),
            (None, Some(body)) => Some(body.to_string().replace("{project}", PROJECT)),
            (None, None) => None,
        };
        if let Some(text) = &mut body {
            for (label, value) in &labels {
                *text = text.replace(&format!("\"<{label}>\""), &format!("\"{value}\""));
            }
        }
        let json = case["headers"].get("Content-Type").is_some() && body.is_some();
        let answer = send(
            addr,
            case["method"].as_str().unwrap(),
            &path,
            body.as_deref(),
            json,
        )
        .await;
        if let Ok(answered) = serde_json::from_str::<Json>(&answer.body) {
            match case.get("defines") {
                Some(Json::String(label)) => {
                    if let Some(Json::String(id)) = answered.get("transaction") {
                        labels.push((label.clone(), id.clone()));
                    }
                }
                Some(Json::Object(paths)) => {
                    for (label, path) in paths {
                        if let Some(Json::String(value)) = at(&answered, path.as_str().unwrap()) {
                            labels.push((label.clone(), value.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        if known.contains(&name) {
            continue;
        }
        let expected = &case["outcome"];
        let actual = normalize(&answer.body);
        if i64::from(answer.status) != expected["status"].as_i64().unwrap()
            || answer.content_type.as_deref() != expected["contentType"].as_str()
            || actual != expected["body"].as_str().unwrap()
        {
            differences.push(format!(
                "{name}:\n    official {} {:?}\n{}\n    hidane   {} {:?}\n{}",
                expected["status"],
                expected["contentType"].as_str(),
                expected["body"].as_str().unwrap(),
                answer.status,
                answer.content_type,
                actual
            ));
        }
    }
    differences
}

#[tokio::test]
async fn datastore_mode_answers_like_the_official_emulator() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/datastore_entities.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap().len();
    assert!(cases > 150);
    let differences = replay(&fixture, KNOWN).await;
    assert!(
        differences.is_empty(),
        "{} of {cases} requests differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// Queries and aggregations in Datastore mode (`fixtures/datastore_queries.json`, from
/// `tools/oracle/datastore_queries.py`).
#[tokio::test]
async fn datastore_queries_answer_like_the_official_emulator() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/datastore_queries.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap().len();
    assert!(cases > 200);
    let differences = replay(&fixture, KNOWN_QUERIES).await;
    assert!(
        differences.is_empty(),
        "{} of {cases} requests differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The same service over gRPC, on the same port, and the Firestore API refused there too.
#[tokio::test]
async fn datastore_mode_serves_grpc_and_refuses_firestore() {
    use hidane_proto::google::{
        datastore::v1::{
            AllocateIdsRequest, CommitRequest, Entity, Key, LookupRequest, Mutation, Value,
            commit_request::Mode, datastore_client::DatastoreClient, key::PathElement,
            mutation::Operation, value::ValueType,
        },
        firestore::v1::{GetDocumentRequest, firestore_client::FirestoreClient},
    };
    use tonic::transport::Channel;

    let addr = start().await;
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut datastore = DatastoreClient::new(channel.clone());
    let incomplete = Key {
        partition_id: None,
        path: vec![PathElement {
            kind: "Task".into(),
            id_type: None,
        }],
    };
    let allocated = datastore
        .allocate_ids(AllocateIdsRequest {
            project_id: "grpc".into(),
            keys: vec![incomplete],
            ..AllocateIdsRequest::default()
        })
        .await
        .unwrap()
        .into_inner()
        .keys;
    assert_eq!(allocated.len(), 1);
    let key = allocated[0].clone();
    let mut entity = Entity {
        key: Some(key.clone()),
        ..Entity::default()
    };
    entity.properties.insert(
        "done".into(),
        Value {
            value_type: Some(ValueType::BooleanValue(true)),
            ..Value::default()
        },
    );
    datastore
        .commit(CommitRequest {
            project_id: "grpc".into(),
            mode: Mode::NonTransactional as i32,
            mutations: vec![Mutation {
                operation: Some(Operation::Upsert(entity)),
                ..Mutation::default()
            }],
            ..CommitRequest::default()
        })
        .await
        .unwrap();
    let found = datastore
        .lookup(LookupRequest {
            project_id: "grpc".into(),
            keys: vec![key],
            ..LookupRequest::default()
        })
        .await
        .unwrap()
        .into_inner()
        .found;
    assert_eq!(found.len(), 1);
    let properties = &found[0].entity.as_ref().unwrap().properties;
    assert_eq!(
        properties["done"].value_type,
        Some(ValueType::BooleanValue(true))
    );

    let refused = FirestoreClient::new(channel)
        .get_document(GetDocumentRequest {
            name: "projects/grpc/databases/(default)/documents/c/d".into(),
            ..GetDocumentRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument);
    assert!(
        refused
            .message()
            .starts_with("Database mode not set to Firestore Mode."),
        "{}",
        refused.message()
    );
}
