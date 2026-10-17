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

/// The GQL queries hidane answers otherwise, on purpose (docs/parity-exceptions.md).
const KNOWN_GQL: &[&str] = &[];

/// The requests of `datastore_protobuf.json` and `datastore_firestore_api.json` hidane answers
/// otherwise, on purpose: a POST whose `Content-Type` is not JSON, which the official emulator
/// answers with a bare 404, is read as JSON (D086).
const KNOWN_CONTENT_TYPES: &[&str] = &[
    "reserveIds, Content-Type 'application/x-protobuf; charset=utf-8'",
    "reserveIds, Content-Type 'APPLICATION/X-PROTOBUF'",
    "reserveIds, Content-Type 'application/JSON'",
    "reserveIds, Content-Type 'application/octet-stream'",
    "reserveIds, Content-Type 'application/protobuf'",
    "reserveIds, Content-Type 'application/x-protobuffer'",
    "reserveIds, Content-Type 'text/x-protobuf'",
    "reserveIds, Content-Type 'application/xml'",
    "reserveIds, Content-Type 'text/html'",
    "reserveIds, Content-Type 'application/x-www-form-urlencoded'",
    "reserveIds, Content-Type 'multipart/form-data'",
    "reserveIds, Content-Type '*/*'",
    "reserveIds, Content-Type 'x'",
    "commit in protobuf",
    "createDocument in protobuf",
    "commit as text/html",
];

/// The requests of `datastore_indexes.json` hidane answers otherwise, on purpose
/// (docs/parity-exceptions.md), as `run: request`.
const KNOWN_INDEXES: &[&str] = &[];

async fn start() -> SocketAddr {
    start_with(hidane::Admin::default().with_datastore_mode(true)).await
}

async fn start_with(admin: hidane::Admin) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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

/// One request; `content_type` is sent as given, `application/json` where the recordings say
/// only that a body is JSON.
async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
    content_type: Option<&str>,
) -> Answer {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(content_type) = content_type {
        request.push_str(&format!("Content-Type: {content_type}\r\n"));
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
fn normalize(text: &str, project: &str) -> String {
    let mut text = text.replace(project, "{project}");
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
    seed(addr, fixture).await;
    replay_cases(
        addr,
        fixture["cases"].as_array().unwrap(),
        known,
        None,
        PROJECT,
    )
    .await
}

/// Writes the fixture's dataset, if it has one.
async fn seed(addr: SocketAddr, fixture: &Json) {
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
                Some("application/json"),
            )
            .await;
            assert_eq!(answer.status, 200, "{}", answer.body);
        }
    }
}

/// Sends each case in order and compares the answers. A step that writes the index file
/// (`write_index_file`) writes `dir/index.yaml`, or removes it.
/// `{project}` in the cases reads `project`, `{dir}` reads `dir`.
async fn replay_cases(
    addr: SocketAddr,
    cases: &[Json],
    known: &[&str],
    dir: Option<&std::path::Path>,
    project: &str,
) -> Vec<String> {
    let shown_dir = dir.map(|d| d.to_str().unwrap().to_owned());
    // Transactions and cursors the server gave, by the names the fixture gives them.
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut differences = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        if let Some(content) = case.get("write_index_file") {
            let path = dir.expect("a run's directory").join("index.yaml");
            match content.as_str() {
                Some(text) => std::fs::write(&path, text).unwrap(),
                None => std::fs::remove_file(&path).unwrap(),
            }
            continue;
        }
        let path = case["path"].as_str().unwrap().replace("{project}", project);
        let mut body = match (case.get("raw"), case.get("body")) {
            (Some(raw), _) => Some(raw.as_str().unwrap().replace("{project}", project)),
            (None, Some(body)) => Some(body.to_string().replace("{project}", project)),
            (None, None) => None,
        };
        if let (Some(text), Some(dir)) = (&mut body, &shown_dir) {
            *text = text.replace("{dir}", dir);
        }
        if let Some(text) = &mut body {
            for (label, value) in &labels {
                *text = text.replace(&format!("\"<{label}>\""), &format!("\"{value}\""));
            }
        }
        let content_type = case["headers"]["Content-Type"]
            .as_str()
            .filter(|_| body.is_some() || case.get("raw").is_none());
        let answer = send(
            addr,
            case["method"].as_str().unwrap(),
            &path,
            body.as_deref(),
            content_type,
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
        let actual = match &shown_dir {
            Some(dir) => normalize(&answer.body.replace(dir.as_str(), "{dir}"), project),
            None => normalize(&answer.body, project),
        };
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

/// GQL in Datastore mode (`fixtures/datastore_gql.json`, from `tools/oracle/datastore_gql.py`):
/// what each query reads as (the answer holds the structured query), its results, and how the
/// queries it cannot read are refused.
#[tokio::test]
async fn gql_answers_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/datastore_gql.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap().len();
    assert!(cases > 100);
    let differences = replay(&fixture, KNOWN_GQL).await;
    assert!(
        differences.is_empty(),
        "{} of {cases} requests differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// `--require_indexes` over gRPC, as the official emulator answers there: a missing index as
/// `FAILED_PRECONDITION`, an index file that cannot be read as `UNKNOWN` without a message.
#[tokio::test]
async fn index_enforcement_over_grpc() {
    use hidane_proto::google::datastore::v1::{
        KindExpression, LookupRequest, PropertyOrder, PropertyReference, Query, RunQueryRequest,
        datastore_client::DatastoreClient, run_query_request::QueryType,
    };
    use tonic::transport::Channel;

    let dir = std::env::temp_dir().join(format!("hidane-datastore-grpc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.yaml"), "indexes:\n- kind: P\n").unwrap();
    let client = |admin: hidane::Admin| async {
        let addr = start_with(admin).await;
        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        DatastoreClient::new(channel)
    };
    let query = |properties: &[&str]| RunQueryRequest {
        project_id: "grpc".into(),
        query_type: Some(QueryType::Query(Query {
            kind: vec![KindExpression { name: "P".into() }],
            order: properties
                .iter()
                .map(|name| PropertyOrder {
                    property: Some(PropertyReference {
                        name: (*name).into(),
                    }),
                    direction: 0,
                })
                .collect(),
            ..Query::default()
        })),
        ..RunQueryRequest::default()
    };
    let admin = hidane::Admin::default().with_datastore_mode(true);
    let mut enforced = client(
        admin
            .clone()
            .with_datastore_indexes(Some(dir.join("index.yaml"))),
    )
    .await;
    assert!(enforced.run_query(query(&[])).await.is_ok());
    let missing = enforced.run_query(query(&["a", "b"])).await.unwrap_err();
    assert_eq!(missing.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        missing.message(),
        "The query requires a composite index that is not defined in your indexes file."
    );
    let mut unread = client(admin.with_datastore_indexes(Some(dir.join("none.yaml")))).await;
    let failed = unread
        .lookup(LookupRequest {
            project_id: "grpc".into(),
            ..LookupRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        (failed.code(), failed.message()),
        (tonic::Code::Unknown, "")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `--require_indexes` (`fixtures/datastore_indexes.json`, from
/// `tools/oracle/datastore_indexes.py`): each run's flags and index file, then its requests:
/// which queries the file's indexes serve, which refusals come before the index and which after,
/// the spellings and mistakes of the file, and when it is read.
#[tokio::test]
async fn indexes_answer_like_the_official_emulator() {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/datastore_indexes.json")).unwrap();
    assert_eq!(fixture["mode"], "datastore-mode");
    let mut cases = 0;
    let mut differences = Vec::new();
    for run in fixture["runs"].as_array().unwrap() {
        let name = run["name"].as_str().unwrap();
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hidane-datastore-indexes-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = run["index_file"].as_str() {
            std::fs::write(dir.join("index.yaml"), text).unwrap();
        }
        let flags: Vec<String> = run["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().replace("{dir}", dir.to_str().unwrap()))
            .collect();
        let mut admin = hidane::Admin::default().with_datastore_mode(true);
        if flags.iter().any(|f| f == "--require_indexes") {
            let file = flags
                .iter()
                .position(|f| f == "--index_file")
                .map(|i| std::path::PathBuf::from(&flags[i + 1]));
            admin = admin.with_datastore_indexes(file);
        }
        let addr = start_with(admin).await;
        if run["seeded"] == true {
            seed(addr, &fixture).await;
        }
        let list = run["cases"].as_array().unwrap();
        cases += list.iter().filter(|c| c.get("outcome").is_some()).count();
        let known: Vec<&str> = KNOWN_INDEXES
            .iter()
            .filter_map(|k| k.strip_prefix(name)?.strip_prefix(": "))
            .collect();
        for difference in replay_cases(addr, list, &known, Some(&dir), PROJECT).await {
            differences.push(format!("[{name}] {difference}"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
    assert!(cases > 400);
    assert!(
        differences.is_empty(),
        "{} of {cases} requests differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// One request with `headers` and a body of bytes; the status, the content type and the body.
async fn send_bytes(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, Option<String>, Vec<u8>) {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if method == "POST" {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    let mut bytes = request.into_bytes();
    if method == "POST" {
        bytes.extend_from_slice(body);
    }
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..end].to_vec()).unwrap();
    let mut body = raw[end + 4..].to_vec();
    let mut lines = head.lines();
    let status = lines.next().unwrap()[9..12].parse().unwrap();
    let mut content_type = None;
    for line in lines {
        let (name, value) = line.split_once(": ").unwrap();
        match name.to_ascii_lowercase().as_str() {
            "content-type" => content_type = Some(value.to_owned()),
            "transfer-encoding" if value == "chunked" => {
                let (mut rest, mut out) = (&body[..], Vec::new());
                loop {
                    let line = rest.windows(2).position(|w| w == b"\r\n").unwrap();
                    let size = std::str::from_utf8(&rest[..line]).unwrap().trim();
                    let size = usize::from_str_radix(size, 16).unwrap();
                    if size == 0 {
                        break;
                    }
                    out.extend_from_slice(&rest[line + 2..line + 2 + size]);
                    rest = &rest[line + 4 + size..];
                }
                body = out;
            }
            _ => {}
        }
    }
    (status, content_type, body)
}

/// REST in protobuf (`fixtures/datastore_protobuf.json`, from
/// `tools/oracle/datastore_protobuf.py`), as the Java and Python client libraries send it over
/// HTTP: which `Content-Type` is read how, each verb's answer, and errors as a
/// `google.rpc.Status`.
#[tokio::test]
async fn protobuf_answers_like_the_official_emulator() {
    use base64::Engine;
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/datastore_protobuf.json")).unwrap();
    assert_eq!(fixture["mode"], "datastore-mode");
    let addr = start().await;
    let b64 = base64::engine::general_purpose::STANDARD;
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() > 50);
    let mut differences = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        if KNOWN_CONTENT_TYPES.contains(&name) {
            continue;
        }
        let headers: Vec<(&str, &str)> = case["headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        let body = b64.decode(case["body"].as_str().unwrap()).unwrap();
        let (status, content_type, answer) = send_bytes(
            addr,
            case["method"].as_str().unwrap(),
            case["path"].as_str().unwrap(),
            &headers,
            &body,
        )
        .await;
        let expected = &case["outcome"];
        let body_differs = case["compare"] == "exact"
            && expected["body"].as_str() != Some(b64.encode(&answer).as_str());
        if i64::from(status) != expected["status"].as_i64().unwrap()
            || content_type.as_deref() != expected["contentType"].as_str()
            || body_differs
        {
            differences.push(format!(
                "{name}:\n    official {} {:?} {:?}\n    hidane   {status} {content_type:?} {:?}",
                expected["status"],
                expected["contentType"].as_str(),
                expected["body"].as_str().map(|b| b64.decode(b).unwrap()),
                String::from_utf8_lossy(&answer)
            ));
        }
    }
    // D086: a body of another type is read as JSON.
    let (status, _, _) = send_bytes(
        addr,
        "POST",
        "/v1/projects/demo-protobuf:reserveIds",
        &[("Content-Type", "text/html")],
        br#"{"keys":[{"path":[{"kind":"T","id":"5"}]}]}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        differences.is_empty(),
        "{} of {} requests differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}

/// An export as the fixture records it: each file relative to the export (its name as `<name>`),
/// an overall metadata file byte for byte, an output file as its records, sorted (D093), any
/// other file as its size.
fn export_tree(root: &std::path::Path, name: &str) -> Json {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut files = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace(name, "<name>");
            let data = std::fs::read(&path).unwrap();
            let file = path.file_name().unwrap().to_str().unwrap();
            let shown = if file.starts_with("output-") {
                let mut records: Vec<String> = hidane_core::export::log::records(&data)
                    .map(|r| b64.encode(r.unwrap()))
                    .collect();
                records.sort();
                serde_json::json!({ "records": records })
            } else if file.ends_with(".overall_export_metadata") {
                serde_json::json!({ "bytes": b64.encode(&data) })
            } else {
                serde_json::json!({ "size": data.len() })
            };
            files.insert(relative, shown);
        }
    }
    serde_json::to_value(files).unwrap()
}

/// Export and import in Datastore mode (`fixtures/datastore_export.json`, from
/// `tools/oracle/datastore_export.py`): each export's files, `--import-data`, `:import`, and the
/// requests refused.
#[tokio::test]
async fn export_and_import_like_the_official_emulator() {
    use base64::Engine;
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/datastore_export.json")).unwrap();
    assert_eq!(fixture["mode"], "datastore-mode");
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut cases = 0;
    let mut differences = Vec::new();
    for (n, run) in fixture["runs"].as_array().unwrap().iter().enumerate() {
        let name = run["name"].as_str().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "hidane-datastore-export-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("out")).unwrap();
        for (relative, data) in fixture["files"].as_object().unwrap() {
            let path = dir.join("files").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b64.decode(data.as_str().unwrap()).unwrap()).unwrap();
        }
        let flags: Vec<String> = run["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().replace("{dir}", dir.to_str().unwrap()))
            .collect();
        let mut admin = hidane::Admin::default().with_datastore_mode(true);
        if let Some(i) = flags.iter().position(|f| f == "--import-data") {
            let entities =
                hidane::export::read_datastore(std::path::Path::new(&flags[i + 1])).unwrap();
            admin = admin.with_datastore_seed(entities);
        }
        let addr = start_with(admin).await;
        let list = run["cases"].as_array().unwrap();
        cases += list.len();
        for difference in replay_cases(addr, list, &[], Some(&dir), "demo-export").await {
            differences.push(format!("[{name}] {difference}"));
        }
        if let Some(exports) = run.get("exports") {
            let mut written = serde_json::Map::new();
            for entry in std::fs::read_dir(dir.join("out")).unwrap() {
                let entry = entry.unwrap().file_name().into_string().unwrap();
                let shown = if entry.starts_with("datastore_export_") {
                    "datastore_export_<seconds>".to_owned()
                } else {
                    entry.clone()
                };
                written.insert(shown, export_tree(&dir.join("out").join(&entry), &entry));
            }
            let written = Json::Object(written);
            if &written != exports {
                differences.push(format!(
                    "[{name}] the exports:\n    official {exports}\n    hidane   {written}"
                ));
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
    assert!(cases > 30);
    assert!(
        differences.is_empty(),
        "{} of {cases} requests differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The Firestore API in Datastore mode (`fixtures/datastore_firestore_api.json`, from
/// `tools/oracle/datastore_firestore_api.py`): routed, read and its names checked, then refused;
/// the emulator's own endpoints failing; over gRPC, a bad name reported before the refusal.
#[tokio::test]
async fn the_firestore_api_is_refused_as_on_the_official_emulator() {
    use hidane_proto::google::firestore::v1::{
        BeginTransactionRequest, CommitRequest, GetDocumentRequest, ListCollectionIdsRequest,
        PartitionQueryRequest, RunQueryRequest, firestore_client::FirestoreClient,
    };
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/datastore_firestore_api.json")).unwrap();
    let addr = start().await;
    let cases = fixture["cases"].as_array().unwrap();
    let mut differences =
        replay_cases(addr, cases, KNOWN_CONTENT_TYPES, None, "demo-firestore-api").await;
    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = FirestoreClient::new(channel);
    let grpc = fixture["grpc"].as_array().unwrap();
    for call in grpc {
        let text = |key: &str| {
            call["request"][key]
                .as_str()
                .unwrap_or_default()
                .replace("{project}", "demo-firestore-api")
        };
        let status = match call["rpc"].as_str().unwrap() {
            "GetDocument" => client
                .get_document(GetDocumentRequest {
                    name: text("name"),
                    ..Default::default()
                })
                .await
                .err(),
            "Commit" => client
                .commit(CommitRequest {
                    database: text("database"),
                    ..Default::default()
                })
                .await
                .err(),
            "ListCollectionIds" => client
                .list_collection_ids(ListCollectionIdsRequest {
                    parent: text("parent"),
                    ..Default::default()
                })
                .await
                .err(),
            "RunQuery" => match client
                .run_query(RunQueryRequest {
                    parent: text("parent"),
                    ..Default::default()
                })
                .await
            {
                Err(status) => Some(status),
                Ok(response) => response.into_inner().message().await.err(),
            },
            "PartitionQuery" => client
                .partition_query(PartitionQueryRequest {
                    parent: text("parent"),
                    ..Default::default()
                })
                .await
                .err(),
            "BeginTransaction" => client
                .begin_transaction(BeginTransactionRequest {
                    database: text("database"),
                    ..Default::default()
                })
                .await
                .err(),
            other => panic!("no call for {other}"),
        };
        let (code, message) = match &status {
            Some(status) => (
                format!("{:?}", status.code()),
                status.message().replace("demo-firestore-api", "{project}"),
            ),
            None => ("Ok".to_owned(), String::new()),
        };
        let expected = &call["outcome"];
        let expected_code = expected["code"]
            .as_str()
            .unwrap()
            .replace('_', "")
            .to_lowercase();
        if code.to_lowercase() != expected_code
            || message != expected["message"].as_str().unwrap_or_default()
        {
            differences.push(format!(
                "{}: official {expected}, hidane {code} {message:?}",
                call["name"]
            ));
        }
    }
    assert!(cases.len() + grpc.len() > 50);
    assert!(
        differences.is_empty(),
        "{} differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
