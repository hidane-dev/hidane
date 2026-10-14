//! Firestore events for the Functions emulator against the official emulator's recordings
//! (`fixtures/function_events.json`, from `tools/oracle/function_events.py`).
//!
//! Each run starts a hidane whose `--functions_emulator` is a server of this test's, which
//! records every request and answers as the recorder's did (`500` for a trigger key holding
//! `fail`, `404` for `missing`, a second's wait for `slow`), or no Functions emulator, or one
//! nothing listens on. Each case's steps are replayed in order: the answer's status, then every
//! request the Functions emulator received (method, path, content type, `ce-*` headers, body)
//! and every `WARNING` about events, step by step.
//!
//! Times and IDs are compared by what they stand for: each distinct commit time, timestamp
//! value and event ID becomes a label in the order it first appears in the case, on each side;
//! `ce-id` must be a fresh UUID and `ce-time` a time to the second. v2 bodies are decoded
//! (`DocumentEventData`) and compared field by field, in order; v1 bodies byte for byte.
//!
//! Intentional differences (docs/parity-exceptions.md): `ce-source` names the project once; a
//! write that changes nothing and the delete of a missing document send no v2 event; a v2
//! trigger registered without a database watches `(default)`; the documents of a deleted path
//! or a cleared database come in name order.

mod common;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hidane_proto::google::firestore::v1::{
    ArrayValue, Document, DocumentMask, MapValue, Value, WriteRequest,
    firestore_client::FirestoreClient, value::ValueType,
};
use prost::Message as _;
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tonic::{Request, transport::Channel};

/// `google.events.cloud.firestore.v1.DocumentEventData`.
#[derive(Clone, PartialEq, prost::Message)]
struct DocumentEventData {
    #[prost(message, optional, tag = "1")]
    value: Option<Document>,
    #[prost(message, optional, tag = "2")]
    old_value: Option<Document>,
    #[prost(message, optional, tag = "3")]
    update_mask: Option<DocumentMask>,
}

// --- The Functions emulator's side -------------------------------------------------------------

#[derive(Debug, Clone)]
struct Received {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Records the requests it receives, and answers as the recorder did.
#[derive(Clone, Default)]
struct Functions {
    received: Arc<Mutex<Vec<Received>>>,
}

impl Functions {
    async fn start(&self) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let functions = self.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let functions = functions.clone();
                tokio::spawn(async move { functions.serve(stream).await });
            }
        });
        host
    }

    async fn serve(&self, stream: TcpStream) {
        let mut stream = BufReader::new(stream);
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let mut parts = line.trim_end().splitn(3, ' ');
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                headers.push((name.to_owned(), value.trim().to_owned()));
            }
            let length = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| value.parse().unwrap());
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            let key = path.rsplit('/').next().unwrap_or_default().to_owned();
            self.received.lock().unwrap().push(Received {
                method,
                path,
                headers,
                body,
            });
            if key.contains("slow") {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let (status, answer) = if key.contains("fail") {
                (
                    "500 Internal Server Error",
                    r#"{"error":"the function failed"}"#,
                )
            } else if key.contains("missing") {
                ("404 Not Found", "Function missing does not exist")
            } else {
                ("200 OK", "{}")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{answer}",
                answer.len()
            );
            if stream.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn take(&self) -> Vec<Received> {
        std::mem::take(&mut *self.received.lock().unwrap())
    }
}

// --- hidane's side -----------------------------------------------------------------------------

struct Emulator {
    addr: SocketAddr,
    admin: hidane::Admin,
    log: Arc<Mutex<Vec<String>>>,
    client: FirestoreClient<Channel>,
}

async fn start(functions: Option<&str>) -> Emulator {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    admin
        .triggers()
        .set_functions_emulator(functions.map(str::to_owned));
    let log = Arc::new(Mutex::new(Vec::new()));
    let records = Arc::clone(&log);
    admin.triggers().set_log(Arc::new(move |record: &str| {
        records.lock().unwrap().push(record.to_owned());
    }));
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin.clone()),
        std::future::pending(),
    ));
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    Emulator {
        addr,
        admin,
        log,
        client: FirestoreClient::new(channel),
    }
}

/// A REST request; the status and the body.
async fn rest(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &Json,
    body: Option<&str>,
) -> (u16, Json) {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n"
    );
    for (name, value) in headers.as_object().into_iter().flatten() {
        request.push_str(&format!("{name}: {}\r\n", value.as_str().unwrap()));
    }
    let body = body.unwrap_or_default();
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
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

/// A gRPC request with the step's headers, or the owner's credential when it has none.
fn with_headers<T>(message: T, database: &str, headers: &Json) -> Request<T> {
    let mut request = Request::new(message);
    let owner = json!({"Authorization": "Bearer owner"});
    let headers = if headers.is_object() { headers } else { &owner };
    for (name, value) in headers.as_object().unwrap() {
        request.metadata_mut().insert(
            tonic::metadata::MetadataKey::from_bytes(name.to_ascii_lowercase().as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    request
        .metadata_mut()
        .insert("google-cloud-resource-prefix", database.parse().unwrap());
    request
}

/// A gRPC step, as grpcurl sent it: the status code's name.
async fn grpc(client: &mut FirestoreClient<Channel>, step: &Json) -> String {
    let database = step["database"].as_str().unwrap();
    let headers = &step["headers"];
    let messages = step["messages"].as_array().unwrap();
    let writes = |message: &Json| {
        common::protojson::commit_request(
            &format!("{database}/documents:commit"),
            &json!({"writes": message["writes"]}),
        )
        .writes
    };
    let result = match step["rpc"].as_str().unwrap() {
        "Commit" => {
            let mut request = common::protojson::commit_request(
                &format!("{database}/documents:commit"),
                &json!({"writes": messages[0]["writes"]}),
            );
            request.database = database.to_owned();
            client
                .commit(with_headers(request, database, headers))
                .await
                .map(|_| ())
                .err()
        }
        "Write" => {
            let requests = vec![
                WriteRequest {
                    database: database.to_owned(),
                    ..WriteRequest::default()
                },
                WriteRequest {
                    database: database.to_owned(),
                    writes: writes(&messages[1]),
                    ..WriteRequest::default()
                },
            ];
            match client
                .write(with_headers(
                    tokio_stream::iter(requests),
                    database,
                    headers,
                ))
                .await
            {
                Ok(response) => {
                    let mut stream = response.into_inner();
                    let mut failure = None;
                    loop {
                        match stream.message().await {
                            Ok(Some(_)) => {}
                            Ok(None) => break,
                            Err(status) => {
                                failure = Some(status);
                                break;
                            }
                        }
                    }
                    failure
                }
                Err(status) => Some(status),
            }
        }
        other => panic!("unknown rpc {other}"),
    };
    match result {
        None => "OK".to_owned(),
        Some(status) => format!("{:?}", status.code()),
    }
}

// --- Normalizing --------------------------------------------------------------------------------

/// Labels for the times and IDs of one side of a case, in the order they first appear.
#[derive(Default)]
struct Labels {
    commit: HashMap<(i64, u32), usize>,
    value: HashMap<(i64, u32), usize>,
    ids: HashMap<String, usize>,
}

fn label<K: std::hash::Hash + Eq>(map: &mut HashMap<K, usize>, key: K) -> usize {
    let next = map.len();
    *map.entry(key).or_insert(next)
}

fn instant(text: &str) -> (i64, u32) {
    let t =
        chrono::DateTime::parse_from_rfc3339(text).unwrap_or_else(|_| panic!("not a time: {text}"));
    (t.timestamp(), t.timestamp_subsec_nanos())
}

fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts.iter().all(|p| {
            p.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        })
        && text.as_bytes()[14] == b'4'
        && matches!(text.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
}

/// The v1 body with each time and event ID replaced by its label.
fn v1_body(body: &str, labels: &mut Labels) -> String {
    let mut out = String::new();
    let mut rest = body;
    loop {
        let next = [
            "\"createTime\":\"",
            "\"updateTime\":\"",
            "\"timestamp\":\"",
            "\"timestampValue\":\"",
            "\"eventId\":\"",
        ]
        .iter()
        .filter_map(|marker| rest.find(marker).map(|at| (at, *marker)))
        .min();
        let Some((at, marker)) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];
        let end = rest.find('"').unwrap();
        let text = &rest[..end];
        let labelled = match marker {
            "\"eventId\":\"" => {
                assert!(is_uuid(text), "eventId {text}");
                format!("<id{}>", label(&mut labels.ids, text.to_owned()))
            }
            "\"timestampValue\":\"" => format!("<v{}>", label(&mut labels.value, instant(text))),
            _ => format!("<t{}>", label(&mut labels.commit, instant(text))),
        };
        out.push_str(&labelled);
        rest = &rest[end..];
    }
}

fn render_value(value: &Value, labels: &mut Labels) -> String {
    match &value.value_type {
        None => "unset".to_owned(),
        Some(ValueType::NullValue(_)) => "null".to_owned(),
        Some(ValueType::BooleanValue(b)) => b.to_string(),
        Some(ValueType::IntegerValue(n)) => format!("{n}i"),
        Some(ValueType::DoubleValue(d)) => format!("{d:?}d"),
        Some(ValueType::TimestampValue(t)) => format!(
            "<v{}>",
            label(
                &mut labels.value,
                (t.seconds, u32::try_from(t.nanos).unwrap())
            )
        ),
        Some(ValueType::StringValue(s)) => format!("{s:?}"),
        Some(ValueType::BytesValue(b)) => format!("b{}", STANDARD.encode(b)),
        Some(ValueType::ReferenceValue(r)) => format!("ref {r:?}"),
        Some(ValueType::GeoPointValue(g)) => format!("geo({:?},{:?})", g.latitude, g.longitude),
        Some(ValueType::ArrayValue(ArrayValue { values })) => format!(
            "[{}]",
            values
                .iter()
                .map(|v| render_value(v, labels))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(ValueType::MapValue(MapValue { fields })) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(k, v)| format!("{k:?}:{}", render_value(v, labels)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(other) => format!("{other:?}"),
    }
}

fn render_document(doc: &Document, labels: &mut Labels) -> String {
    let mut time = |t: &Option<prost_types::Timestamp>| {
        t.as_ref().map_or("none".to_owned(), |t| {
            format!(
                "<t{}>",
                label(
                    &mut labels.commit,
                    (t.seconds, u32::try_from(t.nanos).unwrap())
                )
            )
        })
    };
    let create = time(&doc.create_time);
    let update = time(&doc.update_time);
    let fields = doc
        .fields
        .iter()
        .map(|(k, v)| format!("{k:?}:{}", render_value(v, labels)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{:?} {{{fields}}} {create} {update}}}", doc.name)
}

/// A v2 body, decoded, with each time replaced by its label.
fn v2_body(body: &[u8], labels: &mut Labels) -> String {
    let bytes = STANDARD
        .decode(body)
        .unwrap_or_else(|_| panic!("not base64: {}", String::from_utf8_lossy(body)));
    let data = DocumentEventData::decode(bytes.as_slice()).unwrap();
    let value = data
        .value
        .as_ref()
        .map(|d| render_document(d, labels))
        .unwrap_or_default();
    let old = data
        .old_value
        .as_ref()
        .map(|d| render_document(d, labels))
        .unwrap_or_default();
    format!(
        "value {value} old {old} mask {:?}",
        data.update_mask.map(|m| m.field_paths)
    )
}

/// What a request to the Functions emulator is compared on.
fn normalize(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
    labels: &mut Labels,
) -> Json {
    let mut compared = Vec::new();
    let mut v2 = false;
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if !(name.starts_with("ce-") || name == "content-type") {
            continue;
        }
        let value = match name.as_str() {
            "ce-id" => {
                assert!(is_uuid(value), "ce-id {value}");
                "<uuid>".to_owned()
            }
            "ce-time" => {
                let (_, nanos) = instant(value);
                assert!(nanos == 0 && value.len() == 20, "ce-time {value}");
                "<time>".to_owned()
            }
            // The official emulator writes `projects/` twice.
            "ce-source" => value.replace(
                "//firestore.googleapis.com/projects/projects/",
                "//firestore.googleapis.com/projects/",
            ),
            _ => value.clone(),
        };
        v2 |= name == "ce-type";
        compared.push((name, value));
    }
    compared.sort();
    let body = if v2 {
        v2_body(body, labels)
    } else {
        v1_body(std::str::from_utf8(body).unwrap(), labels)
    };
    json!({"method": method, "path": path, "headers": compared, "body": body})
}

/// Whether the official emulator sent this v2 event for a write that changed nothing or the
/// delete of a missing document, which hidane does not.
fn no_change(delivery: &Json) -> bool {
    let event = &delivery["event"];
    let v2 = delivery["headers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h[0].as_str().unwrap().eq_ignore_ascii_case("ce-type"));
    v2 && (event.get("value") == event.get("oldValue"))
}

/// The document a delivery is about, to order the events of a deleted path or a cleared
/// database by name: v1 first, then v2, each by document (a stable sort keeps the triggers of
/// a document in their order).
fn about(headers: &[(String, String)], body: &[u8]) -> (bool, String) {
    match headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("ce-document"))
    {
        Some((_, document)) => (true, document.clone()),
        None => {
            let body: Json = serde_json::from_slice(body).unwrap();
            let name = body["context"]["resource"]["name"].as_str().unwrap();
            (false, name.rsplit("/documents/").next().unwrap().to_owned())
        }
    }
}

fn received(official: &Json) -> Received {
    let headers = official["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h[0].as_str().unwrap().to_owned(),
                h[1].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    Received {
        method: official["method"].as_str().unwrap().to_owned(),
        path: official["path"].as_str().unwrap().to_owned(),
        headers,
        body: official["body"].as_str().unwrap().as_bytes().to_vec(),
    }
}

// --- Replaying ------------------------------------------------------------------------------------

/// The case whose v2 trigger has no database: the official emulator fails every write of the
/// project with a 500 (after committing it) and sends the v1 events only.
const NO_DATABASE: &str = "a v2 trigger without a database";

/// The case that resets the emulator: the official emulator then forgets every trigger, and
/// sends no more events until the Functions emulator registers them again.
const RESET: &str = "reset";

async fn replay(run: &Json, differences: &mut Vec<String>) -> usize {
    let functions = Functions::default();
    let host = match run["name"].as_str().unwrap() {
        "functions" => Some(functions.start().await),
        "unconfigured" => None,
        // Nothing listens there.
        _ => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            Some(listener.local_addr().unwrap().to_string())
        }
    };
    let mut hidane = start(host.as_deref()).await;
    let work = std::env::temp_dir().join(format!(
        "hidane-function-events-{}-{}",
        std::process::id(),
        run["name"].as_str().unwrap()
    ));
    std::fs::create_dir_all(work.join("export")).unwrap();
    let work = work.to_str().unwrap().to_owned();
    let mut compared = 0;
    for case in run["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut official_labels = Labels::default();
        let mut our_labels = Labels::default();
        let mut transaction = None;
        let mut after_reset = false;
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let context = format!("{} / {name} / step {i}", run["name"]);
            let started = Instant::now();
            let outcome = if step["kind"] == "rest" {
                let mut body = step["body"].clone();
                if let (Some(id), Some(slot)) = (&transaction, body.get_mut("transaction")) {
                    *slot = json!(id);
                }
                let body = (!body.is_null()).then(|| body.to_string().replace("{work}", &work));
                let (status, answer) = rest(
                    hidane.addr,
                    step["method"].as_str().unwrap(),
                    step["path"].as_str().unwrap(),
                    &step["headers"],
                    body.as_deref(),
                )
                .await;
                if let Some(id) = answer.get("transaction").and_then(Json::as_str) {
                    transaction = Some(id.to_owned());
                }
                json!(status)
            } else {
                json!(grpc(&mut hidane.client, step).await)
            };
            let took = started.elapsed();
            hidane.admin.triggers().delivered().await;
            let ours = functions.take();
            let log: Vec<String> = std::mem::take(&mut *hidane.log.lock().unwrap())
                .iter()
                .flat_map(|record| record.lines().map(str::to_owned).collect::<Vec<_>>())
                .map(|line| match &host {
                    Some(host) => line.replace(host.as_str(), "{functions}"),
                    None => line,
                })
                .collect();

            let keys: Vec<&str> = ours
                .iter()
                .map(|r| r.path.rsplit('/').next().unwrap())
                .collect();
            if name == NO_DATABASE && step["status"] == 500 {
                if outcome != 200 || keys != ["v1", "v2-no-database"] {
                    differences.push(format!("{context}: {outcome} {keys:?}"));
                }
                compared += 1;
                continue;
            }
            if name == RESET && after_reset {
                if outcome != step["status"] || keys != ["v1", "v2"] {
                    differences.push(format!("{context}: {outcome} {keys:?}"));
                }
                compared += 1;
                continue;
            }
            after_reset |= step["path"] == "/reset";
            let official_outcome = if step["kind"] == "rest" {
                step["status"].clone()
            } else {
                step["code"].clone()
            };
            if outcome != official_outcome {
                differences.push(format!(
                    "{context}: answered {outcome}, official {official_outcome}"
                ));
            }
            if step["answered_within_a_second"] == true && took >= Duration::from_secs(1) {
                differences.push(format!("{context}: answered after {took:?}"));
            }
            let official_log: Vec<String> = step["log"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|line| line.as_str().unwrap().to_owned())
                .collect();
            if log != official_log {
                differences.push(format!(
                    "{context}: log\n    official {official_log:?}\n    hidane   {log:?}"
                ));
            }

            let mut official: Vec<Received> = step["deliveries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| !no_change(d))
                .map(received)
                .collect();
            let mut ours = ours;
            let unordered = step["method"] == "DELETE"
                && step["path"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("/emulator/v1/"));
            if unordered {
                for list in [&mut official, &mut ours] {
                    list.sort_by_key(|r| about(&r.headers, &r.body));
                }
            }
            let official: Vec<Json> = official
                .iter()
                .map(|r| {
                    normalize(
                        &r.method,
                        &r.path,
                        &r.headers,
                        &r.body,
                        &mut official_labels,
                    )
                })
                .collect();
            let ours: Vec<Json> = ours
                .iter()
                .map(|r| normalize(&r.method, &r.path, &r.headers, &r.body, &mut our_labels))
                .collect();
            if official != ours {
                differences.push(format!(
                    "{context}: deliveries\n    official {}\n    hidane   {}",
                    Json::from(official),
                    Json::from(ours)
                ));
            }
            compared += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    compared
}

#[tokio::test]
async fn events_reach_the_functions_emulator_as_on_the_official_emulator() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/function_events.json")).unwrap();
    let runs = fixture["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 3);
    // The flag changes nothing at startup.
    assert!(runs.iter().all(|run| run["startup"] == runs[0]["startup"]));
    let mut differences = Vec::new();
    let mut compared = 0;
    for run in runs {
        compared += replay(run, &mut differences).await;
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert!(compared > 100, "{compared}");
}

/// Whoever writes, a `withAuthContext` event says `unknown` and the official emulator's
/// placeholder ID; v1 events of one change share their event ID, v2 events do not.
#[tokio::test]
async fn ids_and_auth_context() {
    let functions = Functions::default();
    let host = functions.start().await;
    let hidane = start(Some(&host)).await;
    let p = "demo-ids";
    for (method, path, body) in [
        (
            "PUT",
            format!("/emulator/v1/projects/{p}/triggers/a"),
            json!({"eventTrigger": {"eventType": "providers/cloud.firestore/eventTypes/document.write", "resource": format!("projects/{p}/databases/(default)/documents/c/{{id}}"), "service": "firestore.googleapis.com"}}),
        ),
        (
            "PUT",
            format!("/emulator/v1/projects/{p}/triggers/b"),
            json!({"eventTrigger": {"eventType": "providers/cloud.firestore/eventTypes/document.create", "resource": format!("projects/{p}/databases/(default)/documents/c/{{id}}"), "service": "firestore.googleapis.com"}}),
        ),
        (
            "POST",
            format!("/emulator/v1/projects/{p}/eventarcTrigger?eventarcTriggerId=c"),
            json!({"eventType": "google.cloud.firestore.document.v1.written.withAuthContext", "database": "(default)", "namespace": "(default)", "document": {"value": "c/{id}", "matchType": "PATH_PATTERN"}}),
        ),
        (
            "POST",
            format!("/emulator/v1/projects/{p}/eventarcTrigger?eventarcTriggerId=d"),
            json!({"eventType": "google.cloud.firestore.document.v1.created.withAuthContext", "database": "(default)", "namespace": "(default)", "document": {"value": "c/{id}", "matchType": "PATH_PATTERN"}}),
        ),
    ] {
        let (status, _) = rest(
            hidane.addr,
            method,
            &path,
            &json!({}),
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, 200);
    }
    let user = "Bearer eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1In0.";
    let (status, _) = rest(
        hidane.addr,
        "PATCH",
        &format!("/v1/projects/{p}/databases/(default)/documents/c/a"),
        &json!({"Authorization": user}),
        Some(r#"{"fields": {}}"#),
    )
    .await;
    assert_eq!(status, 200);
    hidane.admin.triggers().delivered().await;
    let received = functions.take();
    assert_eq!(received.len(), 4);
    let header = |r: &Received, name: &str| {
        r.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let event_id = |r: &Received| {
        let body: Json = serde_json::from_slice(&r.body).unwrap();
        body["context"]["eventId"].as_str().unwrap().to_owned()
    };
    assert_eq!(event_id(&received[0]), event_id(&received[1]));
    assert_ne!(header(&received[2], "ce-id"), header(&received[3], "ce-id"));
    for r in &received[2..] {
        assert_eq!(header(r, "ce-authtype").as_deref(), Some("unknown"));
        assert_eq!(
            header(r, "ce-authid").as_deref(),
            Some("fake-auth-id@gmail.com")
        );
    }
}
