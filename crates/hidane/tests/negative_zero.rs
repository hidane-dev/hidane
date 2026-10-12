//! `-0.0` against the official emulator's recordings (`fixtures/negative_zero.json`, from
//! `tools/oracle/negative_zero.py`): written over gRPC, REST and WebChannel, read back over
//! each, through field transforms, filters, orderings, cursors, aggregations, Security Rules and
//! an export and import. The store keeps `-0.0`, and writing it over `0.0` changes the document
//! (NaN over NaN does not); REST and WebChannel read every zero as `0.0` and print `-0.0` as
//! `-0.0`.
//!
//! The steps run in order against one hidane. Answers are summarised as the oracle summarises
//! them, each double spelled as text, so the sign of zero is compared. The steps that write a
//! literal which only rounds to zero (`-1e-400`) are the exception in
//! docs/parity-exceptions.md: hidane reads it as `0.0`, where the official emulator keeps `-0.0`.

use std::{
    fs,
    net::SocketAddr,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use hidane_proto::{
    google::firestore::v1::{
        BatchGetDocumentsRequest, CommitRequest, CommitResponse, CreateDocumentRequest, Document,
        GetDocumentRequest, ListenRequest, RunAggregationQueryRequest, RunQueryRequest,
        UpdateDocumentRequest, Value, WriteRequest, WriteResponse, batch_get_documents_response,
        firestore_client::FirestoreClient, listen_response, target_change::TargetChangeType,
        value::ValueType,
    },
    ordered::Fields,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Status, transport::Channel};

type Client = FirestoreClient<Channel>;

async fn start() -> (Client, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    (FirestoreClient::new(channel), addr)
}

fn database(project: &str) -> String {
    format!("projects/{project}/databases/(default)")
}

fn documents(project: &str) -> String {
    format!("{}/documents", database(project))
}

// Summaries, as the oracle makes them --------------------------------------------------------

/// A double as text, in the spelling of the oracle's Python (`repr`).
fn spell(d: f64) -> String {
    if d.is_nan() {
        "NaN".to_owned()
    } else {
        format!("{d:?}")
    }
}

fn value(v: &Value) -> Json {
    match v.value_type.as_ref().expect("a value") {
        ValueType::DoubleValue(d) => json!({"double": spell(*d)}),
        ValueType::IntegerValue(i) => json!({"integer": i.to_string()}),
        ValueType::NullValue(_) => json!({"nullValue": "NULL_VALUE"}),
        ValueType::ArrayValue(array) => {
            json!({"array": array.values.iter().map(value).collect::<Vec<_>>()})
        }
        ValueType::MapValue(map) => json!({"map": fields(&map.fields)}),
        ValueType::GeoPointValue(point) => {
            json!({"geo": [spell(point.latitude), spell(point.longitude)]})
        }
        other => panic!("unexpected value {other:?}"),
    }
}

fn fields(fields: &Fields) -> Json {
    Json::Object(fields.iter().map(|(k, v)| (k.clone(), value(v))).collect())
}

fn document(doc: &Document, project: &str) -> Json {
    let id = doc.name.strip_prefix(&format!("{}/", documents(project)));
    json!({"id": id.unwrap(), "fields": fields(&doc.fields)})
}

/// The same, from ProtoJSON whose doubles are the literals printed (see [`literal_doubles`]).
fn json_value(v: &Json) -> Json {
    let (kind, inner) = v.as_object().unwrap().iter().next().unwrap();
    let literal = |d: Option<&Json>| d.map_or("0.0", |d| d.as_str().unwrap()).to_owned();
    match kind.as_str() {
        "doubleValue" => json!({"double": literal(Some(inner))}),
        "integerValue" => json!({"integer": inner}),
        "arrayValue" => json!({"array": inner["values"]
            .as_array()
            .map(|values| values.iter().map(json_value).collect())
            .unwrap_or_else(Vec::new)}),
        "mapValue" => json!({"map": json_fields(&inner["fields"])}),
        "geoPointValue" => {
            json!({"geo": [literal(inner.get("latitude")), literal(inner.get("longitude"))]})
        }
        _ => Json::Object([(kind.clone(), inner.clone())].into_iter().collect()),
    }
}

fn json_fields(fields: &Json) -> Json {
    Json::Object(
        fields
            .as_object()
            .map(|f| f.iter().map(|(k, v)| (k.clone(), json_value(v))).collect())
            .unwrap_or_default(),
    )
}

fn json_document(doc: &Json, project: &str) -> Json {
    let name = doc["name"].as_str().unwrap();
    let id = name.strip_prefix(&format!("{}/", documents(project)));
    json!({"id": id.unwrap(), "fields": json_fields(&doc["fields"])})
}

/// ProtoJSON with every double as the literal printed, as a string, so `-0.0` and `0.0` stay
/// apart (the oracle's `literal_doubles`).
fn literal_doubles(text: &str) -> Json {
    const KEYS: [&str; 3] = ["\"doubleValue\":", "\"latitude\":", "\"longitude\":"];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, len)) = KEYS
        .iter()
        .filter_map(|key| rest.find(key).map(|at| (at, key.len())))
        .min()
    {
        out.push_str(&rest[..at + len]);
        rest = &rest[at + len..];
        let spaces = rest.len() - rest.trim_start_matches(' ').len();
        out.push_str(&rest[..spaces]);
        rest = &rest[spaces..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || "-+.eE".contains(c)))
            .unwrap_or(rest.len());
        if end > 0 {
            out.push('"');
            out.push_str(&rest[..end]);
            out.push('"');
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    serde_json::from_str(&out).unwrap()
}

fn error(status: &Status) -> Json {
    let mut name = String::new();
    for (i, c) in format!("{:?}", status.code()).chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    json!({"error": name})
}

fn commit_summary(response: &CommitResponse) -> Json {
    let results: Vec<Json> = response
        .write_results
        .iter()
        .map(|r| {
            json!({
                "transformResults": r.transform_results.iter().map(value).collect::<Vec<_>>(),
                "changed": r.update_time == response.commit_time,
            })
        })
        .collect();
    json!({"writeResults": results})
}

fn write_summary(responses: &[WriteResponse]) -> Json {
    let results: Vec<Json> = responses
        .iter()
        .flat_map(|r| &r.write_results)
        .map(|r| json!({"transformResults": r.transform_results.iter().map(value).collect::<Vec<_>>()}))
        .collect();
    json!({"writeResults": results})
}

/// ProtoJSON answers (REST, WebChannel), summarised as `kind` says.
fn json_summary(kind: &str, answer: &[Json], project: &str) -> Json {
    let documents_of = |key: &str| -> Vec<&Json> {
        answer
            .iter()
            .filter_map(|r| r.get(key))
            .map(|r| {
                if key == "documentChange" {
                    &r["document"]
                } else {
                    r
                }
            })
            .collect()
    };
    match kind {
        "commit" => {
            let body = &answer[0];
            let results: Vec<Json> = body["writeResults"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let transforms = r["transformResults"]
                        .as_array()
                        .map(|vs| vs.iter().map(json_value).collect())
                        .unwrap_or_else(Vec::new);
                    json!({"transformResults": transforms, "changed": r["updateTime"] == body["commitTime"]})
                })
                .collect();
            json!({"writeResults": results})
        }
        "document" => json_document(&answer[0], project),
        "batchGet" => json!({
            "found": documents_of("found").into_iter().map(|d| json_document(d, project)).collect::<Vec<_>>(),
            "missing": documents_of("missing")
                .into_iter()
                .map(|m| m.as_str().unwrap().strip_prefix(&format!("{}/", documents(project))).unwrap())
                .collect::<Vec<_>>(),
        }),
        "queryFields" => json!({"documents": documents_of("document")
            .into_iter()
            .map(|d| json_document(d, project))
            .collect::<Vec<_>>()}),
        "listen" => json!({"documents": documents_of("documentChange")
            .into_iter()
            .map(|d| json_document(d, project))
            .collect::<Vec<_>>()}),
        "write" => {
            let results: Vec<Json> = answer
                .iter()
                .filter_map(|r| r["writeResults"].as_array())
                .flatten()
                .map(|r| {
                    let transforms = r["transformResults"]
                        .as_array()
                        .map(|vs| vs.iter().map(json_value).collect())
                        .unwrap_or_else(Vec::new);
                    json!({"transformResults": transforms})
                })
                .collect();
            json!({"writeResults": results})
        }
        other => panic!("no ProtoJSON summary {other}"),
    }
}

// gRPC ---------------------------------------------------------------------------------------

fn message<T: prost::Message + Default>(name: &str, json: &Json) -> T {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    let pool =
        POOL.get_or_init(|| DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).unwrap());
    let descriptor = pool
        .get_message_by_name(&format!("google.firestore.v1.{name}"))
        .unwrap();
    DynamicMessage::deserialize(descriptor, json.clone())
        .unwrap()
        .transcode_to()
        .unwrap()
}

fn request<T>(message: T, owner: bool) -> Request<T> {
    let mut request = Request::new(message);
    if owner {
        request
            .metadata_mut()
            .insert("authorization", "Bearer owner".parse().unwrap());
    }
    request
}

async fn collect<T>(
    response: Result<tonic::Response<tonic::Streaming<T>>, Status>,
) -> Result<Vec<T>, Status> {
    let mut stream = response?.into_inner();
    let mut out = Vec::new();
    while let Some(message) = stream.message().await? {
        out.push(message);
    }
    Ok(out)
}

async fn grpc(client: &mut Client, step: &Json) -> Json {
    let project = step["project"].as_str().unwrap();
    let rpc = step["grpc"].as_str().unwrap();
    let owner = step["owner"] != false;
    let kind = step["summary"].as_str().unwrap();
    let json = &step["request"];
    let document_summary = |answer: Result<tonic::Response<Document>, Status>| match answer {
        Ok(doc) => document(doc.get_ref(), project),
        Err(status) => error(&status),
    };
    match rpc {
        "Commit" => {
            let r = message::<CommitRequest>("CommitRequest", json);
            match client.commit(request(r, owner)).await {
                Ok(response) => commit_summary(response.get_ref()),
                Err(status) => error(&status),
            }
        }
        "GetDocument" => {
            let r = message::<GetDocumentRequest>("GetDocumentRequest", json);
            document_summary(client.get_document(request(r, owner)).await)
        }
        "CreateDocument" => {
            let r = message::<CreateDocumentRequest>("CreateDocumentRequest", json);
            document_summary(client.create_document(request(r, owner)).await)
        }
        "UpdateDocument" => {
            let r = message::<UpdateDocumentRequest>("UpdateDocumentRequest", json);
            document_summary(client.update_document(request(r, owner)).await)
        }
        "BatchGetDocuments" => {
            let r = message::<BatchGetDocumentsRequest>("BatchGetDocumentsRequest", json);
            match collect(client.batch_get_documents(request(r, owner)).await).await {
                Err(status) => error(&status),
                Ok(responses) => {
                    let (mut found, mut missing) = (Vec::new(), Vec::new());
                    for response in responses {
                        match response.result {
                            Some(batch_get_documents_response::Result::Found(doc)) => {
                                found.push(document(&doc, project));
                            }
                            Some(batch_get_documents_response::Result::Missing(name)) => {
                                let prefix = format!("{}/", documents(project));
                                missing.push(json!(name.strip_prefix(&prefix).unwrap()));
                            }
                            None => {}
                        }
                    }
                    json!({"found": found, "missing": missing})
                }
            }
        }
        "RunQuery" => {
            let r = message::<RunQueryRequest>("RunQueryRequest", json);
            match collect(client.run_query(request(r, owner)).await).await {
                Err(status) => error(&status),
                Ok(responses) => {
                    let docs = responses
                        .iter()
                        .filter_map(|r| r.document.as_ref())
                        .map(|d| document(d, project));
                    if kind == "query" {
                        json!({"documents": docs.map(|d| d["id"].clone()).collect::<Vec<_>>()})
                    } else {
                        json!({"documents": docs.collect::<Vec<_>>()})
                    }
                }
            }
        }
        "RunAggregationQuery" => {
            let r = message::<RunAggregationQueryRequest>("RunAggregationQueryRequest", json);
            match collect(client.run_aggregation_query(request(r, owner)).await).await {
                Err(status) => error(&status),
                Ok(responses) => {
                    let result = responses.iter().find_map(|r| r.result.as_ref()).unwrap();
                    json!({"result": fields(&result.aggregate_fields)})
                }
            }
        }
        "Listen" => listen(client, step, project, owner).await,
        "Write" => write(client, step, owner).await,
        other => panic!("no RPC {other}"),
    }
}

async fn listen(client: &mut Client, step: &Json, project: &str, owner: bool) -> Json {
    let (tx, rx) = mpsc::channel(8);
    for m in step["stream"].as_array().unwrap() {
        tx.send(message::<ListenRequest>("ListenRequest", m))
            .await
            .unwrap();
    }
    let mut stream = client
        .listen(request(ReceiverStream::new(rx), owner))
        .await
        .unwrap()
        .into_inner();
    let mut docs = Vec::new();
    loop {
        let response = tokio::time::timeout(Duration::from_secs(5), stream.message())
            .await
            .expect("CURRENT in time")
            .unwrap()
            .expect("the stream stays open");
        match response.response_type {
            Some(listen_response::ResponseType::DocumentChange(change)) => {
                docs.push(document(change.document.as_ref().unwrap(), project));
            }
            Some(listen_response::ResponseType::TargetChange(change))
                if change.target_change_type == TargetChangeType::Current as i32 =>
            {
                break;
            }
            _ => {}
        }
    }
    drop(tx);
    json!({"documents": docs})
}

async fn write(client: &mut Client, step: &Json, owner: bool) -> Json {
    let messages = step["stream"].as_array().unwrap();
    let (tx, rx) = mpsc::channel(8);
    tx.send(message::<WriteRequest>("WriteRequest", &messages[0]))
        .await
        .unwrap();
    let mut stream = client
        .write(request(ReceiverStream::new(rx), owner))
        .await
        .unwrap()
        .into_inner();
    stream.message().await.unwrap().expect("the handshake");
    let mut responses = Vec::new();
    for m in &messages[1..] {
        tx.send(message::<WriteRequest>("WriteRequest", m))
            .await
            .unwrap();
        responses.push(stream.message().await.unwrap().expect("an answer"));
    }
    write_summary(&responses)
}

// HTTP: REST, WebChannel, the emulator's own endpoints ---------------------------------------

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    let chunked = head
        .lines()
        .any(|l| l.eq_ignore_ascii_case("transfer-encoding: chunked"));
    (
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_owned()
        },
    )
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

async fn rest(addr: SocketAddr, step: &Json) -> (u16, Json) {
    let project = step["project"].as_str().unwrap();
    let path = format!(
        "/v1/{}{}",
        documents(project),
        step["path"].as_str().unwrap()
    );
    let mut headers = vec![("Content-Type", "application/json")];
    if step["owner"] != false {
        headers.push(("Authorization", "Bearer owner"));
    }
    let body = step["body"].as_str().unwrap_or_default();
    let (status, text) = http(addr, step["rest"].as_str().unwrap(), &path, &headers, body).await;
    if status != 200 {
        let body: Json = serde_json::from_str(&text).unwrap();
        return (status, json!({"error": body["error"]["status"]}));
    }
    let answer = match literal_doubles(&text) {
        Json::Array(answers) => answers,
        one => vec![one],
    };
    let kind = step["summary"].as_str().unwrap();
    (status, json_summary(kind, &answer, project))
}

fn encode(text: &str) -> String {
    utf8_percent_encode(text, NON_ALPHANUMERIC).to_string()
}

fn form(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The JSON of each `<length>\n<JSON>` frame, the length in UTF-16 code units.
fn frames(mut body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    while let Some((length, rest)) = body.split_once('\n') {
        let length: usize = length.parse().unwrap();
        let (mut units, mut end) = (0, 0);
        for c in rest.chars() {
            if units == length {
                break;
            }
            units += c.len_utf16();
            end += c.len_utf8();
        }
        out.push(&rest[..end]);
        body = &rest[end..];
    }
    out
}

/// The oracle's `webchannel`: a session opened with the first message, the others sent on the
/// forward channel, then long-polling back channels until `CURRENT` (Listen) or the answer to
/// the last write (Write). Returns the messages received, without `noop`s.
async fn webchannel(addr: SocketAddr, step: &Json) -> Json {
    let project = step["project"].as_str().unwrap();
    let rpc = step["webchannel"].as_str().unwrap();
    let messages: Vec<&str> = step["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    let db = database(project);
    let path = format!("/google.firestore.v1.Firestore/{rpc}/channel");
    let form_headers = [("Content-Type", "application/x-www-form-urlencoded")];
    let query = form(&[
        ("VER", "8".into()),
        ("database", db.clone()),
        ("RID", "1000".into()),
        ("CVER", "22".into()),
        ("X-HTTP-Session-Id", "gsessionid".into()),
    ]);
    let body = form(&[
        (
            "headers",
            "X-Goog-Api-Client:gl-js/ fire/13.0.0\r\nContent-Type:text/plain\r\n".into(),
        ),
        ("count", "1".into()),
        ("ofs", "0".into()),
        ("req0___data__", messages[0].into()),
    ]);
    let (status, text) = http(
        addr,
        "POST",
        &format!("{path}?{query}"),
        &form_headers,
        &body,
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let handshake: Json = serde_json::from_str(frames(&text)[0]).unwrap();
    let sid = handshake[0][1][1].as_str().unwrap().to_owned();
    for (n, message) in messages[1..].iter().enumerate() {
        let query = form(&[
            ("VER", "8".into()),
            ("database", db.clone()),
            ("SID", sid.clone()),
            ("RID", (1001 + n).to_string()),
            ("AID", "0".into()),
        ]);
        let body = form(&[
            ("count", "1".into()),
            ("ofs", (1 + n).to_string()),
            ("req0___data__", (*message).to_owned()),
        ]);
        let (status, _) = http(
            addr,
            "POST",
            &format!("{path}?{query}"),
            &form_headers,
            &body,
        )
        .await;
        assert_eq!(status, 200);
    }
    let until = if rpc == "Listen" {
        "CURRENT"
    } else {
        "writeResults"
    };
    let (mut received, mut aid) = (Vec::new(), 0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !received
        .iter()
        .any(|r: &Json| r.to_string().contains(until))
    {
        assert!(tokio::time::Instant::now() < deadline, "no {until}");
        let query = form(&[
            ("VER", "8".into()),
            ("database", db.clone()),
            ("RID", "rpc".into()),
            ("SID", sid.clone()),
            ("AID", aid.to_string()),
            ("CI", "1".into()),
            ("TYPE", "xmlhttp".into()),
        ]);
        let (_, text) = http(addr, "GET", &format!("{path}?{query}"), &[], "").await;
        for frame in frames(&text) {
            for entry in literal_doubles(frame).as_array().unwrap() {
                aid = entry[0].as_u64().unwrap();
                let message = &entry[1][0];
                if message != "noop" {
                    received.push(message.clone());
                }
            }
        }
    }
    if rpc == "Write" {
        received.remove(0); // the handshake
    }
    json_summary(step["summary"].as_str().unwrap(), &received, project)
}

/// Exports `step`'s project, counts the doubles that are `-0.0` in its documents' file, and
/// imports it into the project the step names.
async fn export_and_import(addr: SocketAddr, step: &Json) -> Json {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "hidane-negative-zero-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let project = step["project"].as_str().unwrap();
    let headers = [("Content-Type", "application/json")];
    let body = json!({
        "database": database(project),
        "export_directory": dir.to_str().unwrap(),
        "export_name": "e",
    });
    let path = format!("/emulator/v1/projects/{project}:export");
    let (status, text) = http(addr, "POST", &path, &headers, &body.to_string()).await;
    assert_eq!(status, 200, "{text}");
    let output = fs::read(dir.join("e/all_namespaces/all_kinds/output-0")).unwrap();
    let negative_zero = (-0.0f64).to_le_bytes();
    let count = output.windows(8).filter(|w| *w == negative_zero).count();
    let target = step["export"].as_str().unwrap();
    let body = json!({
        "database": database(target),
        "export_directory": dir.join("e/e.overall_export_metadata").to_str().unwrap(),
    });
    let path = format!("/emulator/v1/projects/{target}:import");
    let (status, text) = http(addr, "POST", &path, &headers, &body.to_string()).await;
    assert_eq!(status, 200, "{text}");
    fs::remove_dir_all(&dir).unwrap();
    json!({"negative_zero_doubles": count})
}

/// The official outcome with every `-0.0` read as `0.0`, for the literals that only round to
/// zero (docs/parity-exceptions.md).
fn positive(outcome: &Json) -> Json {
    match outcome {
        Json::String(s) if s == "-0.0" => json!("0.0"),
        Json::Array(values) => Json::Array(values.iter().map(positive).collect()),
        Json::Object(map) => {
            Json::Object(map.iter().map(|(k, v)| (k.clone(), positive(v))).collect())
        }
        other => other.clone(),
    }
}

#[tokio::test]
async fn every_recorded_step_gives_the_official_answer() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/negative_zero.json")).unwrap();
    let (mut client, addr) = start().await;
    let steps = fixture["steps"].as_array().unwrap();
    for step in steps {
        let name = step["name"].as_str().unwrap();
        let project = step["project"].as_str().unwrap();
        if let Some(rules) = step.get("rules") {
            let body = json!({"rules": {"files": [{"name": "firestore.rules", "content": rules}]}});
            let path = format!("/emulator/v1/projects/{project}:securityRules");
            let headers = [("Content-Type", "application/json")];
            let (status, text) = http(addr, "PUT", &path, &headers, &body.to_string()).await;
            assert_eq!(json!(status), step["status"], "{name}: {text}");
            continue;
        }
        let got = if step.get("grpc").is_some() {
            grpc(&mut client, step).await
        } else if step.get("rest").is_some() {
            let (status, got) = rest(addr, step).await;
            assert_eq!(json!(status), step["status"], "{name}");
            got
        } else if step.get("webchannel").is_some() {
            webchannel(addr, step).await
        } else if step.get("export").is_some() {
            export_and_import(addr, step).await
        } else {
            panic!("{name}: unknown step");
        };
        let want = if name.contains("-1e-400") {
            positive(&step["outcome"])
        } else {
            step["outcome"].clone()
        };
        assert_eq!(got, want, "{name}");
    }
    assert!(steps.len() > 90, "replayed only {} steps", steps.len());
}
