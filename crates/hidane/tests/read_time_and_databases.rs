//! How old a `read_time` may be and how many databases a project may have, against the official
//! emulator's recordings (`fixtures/read_time_and_databases.json`, from
//! `tools/oracle/read_time_and_databases.py`, #86).
//!
//! Each read of the fixture is made at `now - age` for every recorded age, over gRPC and REST,
//! and compared with the official answer after the oracle's masking. Two answers differ on
//! purpose (docs/parity-exceptions.md): hidane keeps one hour of versions where the official
//! emulator serves seven days (D065), so a read between the two answers "too old", as the
//! official emulator does past seven days; and the REST `?readTime=` parameter, which the
//! official emulator always refuses, reads as the gRPC GetDocument of the same document does
//! (D066). The databases are written as recorded: none is refused.

mod common;

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use common::structured_query::structured_query;
use hidane_proto::google::firestore::v1::{
    GetDocumentRequest, ListDocumentsRequest, RunQueryRequest, firestore_client::FirestoreClient,
    get_document_request, list_documents_request, run_query_request,
};
use prost_types::Timestamp;
use serde_json::{Map, Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tonic::{Code, Request, Status, transport::Channel};

type Client = FirestoreClient<Channel>;

/// How old a read time hidane serves: the versions it keeps (ADR 0002, "Retention").
const HIDANE_WINDOW: u64 = 60 * 60;

fn fixture() -> Json {
    serde_json::from_str(include_str!("fixtures/read_time_and_databases.json")).unwrap()
}

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

fn owner<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", "Bearer owner".parse().unwrap());
    request
}

/// `now - age`, as a timestamp and as RFC 3339 with microseconds.
fn before_now(age: u64) -> (Timestamp, String) {
    let at = SystemTime::now() - Duration::from_secs(age);
    let since = at.duration_since(UNIX_EPOCH).unwrap();
    let micros = since.subsec_micros();
    let timestamp = Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap(),
        nanos: i32::try_from(micros * 1_000).unwrap(),
    };
    let text = chrono::DateTime::from_timestamp(timestamp.seconds, micros * 1_000)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string();
    (timestamp, text)
}

/// One HTTP/1.1 request; the status and the body (de-chunked).
async fn send(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let body = body.unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer owner\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
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

/// The oracle's masking: server times become `<time>`, transaction IDs `<id>`.
fn masked(value: Json) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = match (k.as_str(), v) {
                        (
                            "createTime" | "updateTime" | "readTime" | "commitTime",
                            Json::String(_),
                        ) => json!("<time>"),
                        ("transaction", Json::String(_)) => json!("<id>"),
                        (_, v) => masked(v),
                    };
                    (k, v)
                })
                .collect::<Map<_, _>>(),
        ),
        Json::Array(items) => Json::Array(items.into_iter().map(masked).collect()),
        other => other,
    }
}

/// `FailedPrecondition` → `FAILED_PRECONDITION`, as the oracle records grpcurl's codes.
fn code_name(code: Code) -> String {
    format!("{code:?}")
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            let sep = (i > 0 && c.is_ascii_uppercase()).then_some('_');
            sep.into_iter()
                .chain(std::iter::once(c.to_ascii_uppercase()))
        })
        .collect()
}

fn relative(name: &str) -> Json {
    json!(name.split_once("/documents/").unwrap().1)
}

fn grpc_answer(outcome: Result<Vec<Json>, Status>) -> Json {
    match outcome {
        Ok(documents) => json!({"status": "OK", "message": "", "documents": documents}),
        Err(status) => json!({"status": code_name(status.code()), "message": status.message()}),
    }
}

async fn grpc(client: &mut Client, read: &Json, at: Timestamp) -> Json {
    let body = &read["body"];
    let text = |key: &str| body[key].as_str().unwrap().to_owned();
    let outcome = match read["method"].as_str().unwrap() {
        "GetDocument" => client
            .get_document(owner(GetDocumentRequest {
                name: text("name"),
                consistency_selector: Some(get_document_request::ConsistencySelector::ReadTime(at)),
                ..GetDocumentRequest::default()
            }))
            .await
            .map(|r| vec![relative(&r.into_inner().name)]),
        "ListDocuments" => client
            .list_documents(owner(ListDocumentsRequest {
                parent: text("parent"),
                collection_id: text("collectionId"),
                consistency_selector: Some(list_documents_request::ConsistencySelector::ReadTime(
                    at,
                )),
                ..ListDocumentsRequest::default()
            }))
            .await
            .map(|r| {
                r.into_inner()
                    .documents
                    .iter()
                    .map(|d| relative(&d.name))
                    .collect()
            }),
        "RunQuery" => {
            let request = RunQueryRequest {
                parent: text("parent"),
                query_type: Some(run_query_request::QueryType::StructuredQuery(
                    structured_query(&body["structuredQuery"]),
                )),
                consistency_selector: Some(run_query_request::ConsistencySelector::ReadTime(at)),
                ..RunQueryRequest::default()
            };
            match client.run_query(owner(request)).await {
                Err(status) => Err(status),
                Ok(stream) => {
                    let mut stream = stream.into_inner();
                    let mut documents = Vec::new();
                    loop {
                        match stream.message().await {
                            Ok(Some(r)) => {
                                documents.extend(r.document.map(|d| relative(&d.name)));
                            }
                            Ok(None) => break Ok(documents),
                            Err(status) => break Err(status),
                        }
                    }
                }
            }
        }
        other => panic!("no gRPC call for {other}"),
    };
    grpc_answer(outcome)
}

async fn rest(addr: SocketAddr, read: &Json, at: &str) -> Json {
    let body =
        (!read["body"].is_null()).then(|| read["body"].to_string().replace("{readTime}", at));
    let path = read["path"].as_str().unwrap().replace("{readTime}", at);
    let (status, text) = send(
        addr,
        read["method"].as_str().unwrap(),
        &path,
        body.as_deref(),
    )
    .await;
    let response = if text.is_empty() {
        Json::Null
    } else {
        masked(serde_json::from_str(&text).unwrap())
    };
    json!({"status": status, "response": response})
}

/// What hidane is expected to answer for `read` at `age`, given the official answers of every
/// read at every age (`cases`).
fn expected(cases: &[Json], reads: &[Json], case: usize, read: usize) -> Json {
    let age = cases[case]["age"].as_u64().unwrap();
    let official = &cases[case]["answers"][read];
    let too_old = |read: usize| {
        // The official answer past seven days.
        let past = cases.last().unwrap();
        let answer = past["answers"][read].clone();
        assert!(
            answer.to_string().contains("too old"),
            "{answer} is not too old"
        );
        answer
    };
    if reads[read]["name"] == "REST GET ?readTime=" {
        // D066: the official emulator refuses the parameter at every age; hidane reads as gRPC
        // GetDocument of the same document does.
        assert_eq!(
            official["response"]["error"]["message"],
            "Only timestamps past epoch are supported."
        );
        let get = reads
            .iter()
            .position(|r| r["name"] == "GetDocument of the written document")
            .unwrap();
        let get = expected(cases, reads, case, get);
        return match get["status"].as_str().unwrap() {
            "OK" => json!({"status": 200, "document": true}),
            "NOT_FOUND" => {
                json!({"status": 404, "message": get["message"], "error": "NOT_FOUND"})
            }
            "FAILED_PRECONDITION" => {
                json!({"status": 400, "message": get["message"], "error": "FAILED_PRECONDITION"})
            }
            other => panic!("GetDocument answered {other}"),
        };
    }
    if age > HIDANE_WINDOW && !official.to_string().contains("too old") {
        // D065: one hour of versions, not seven days.
        return too_old(read);
    }
    official.clone()
}

/// A REST `GET ?readTime=` answer in the shape `expected` gives it.
fn summarised(answer: &Json) -> Json {
    let status = answer["status"].as_u64().unwrap();
    if status == 200 {
        return json!({"status": 200, "document": answer["response"]["name"].is_string()});
    }
    let error = &answer["response"]["error"];
    json!({"status": status, "message": error["message"], "error": error["status"]})
}

#[tokio::test]
async fn read_times_up_to_the_window() {
    let fixture = fixture();
    let reads = fixture["read_time"]["reads"].as_array().unwrap();
    let cases = fixture["read_time"]["cases"].as_array().unwrap();
    let (mut client, addr) = start().await;
    let docs = "projects/read-time/databases/(default)/documents";
    let write = json!({"writes": [{"update": {"name": format!("{docs}/c/d"), "fields": {"a": {"integerValue": "1"}}}}]});
    let (status, text) = send(
        addr,
        "POST",
        &format!("/v1/{docs}:commit"),
        Some(&write.to_string()),
    )
    .await;
    assert_eq!(status, 200, "{text}");

    let mut differences = Vec::new();
    for (c, case) in cases.iter().enumerate() {
        let age = case["age"].as_u64().unwrap();
        for (r, read) in reads.iter().enumerate() {
            let (timestamp, text) = before_now(age);
            let mut actual = match read["transport"].as_str().unwrap() {
                "grpc" => grpc(&mut client, read, timestamp).await,
                _ => rest(addr, read, &text).await,
            };
            if read["name"] == "REST GET ?readTime=" {
                actual = summarised(&actual);
            }
            let expected = expected(cases, reads, c, r);
            if actual != expected {
                differences.push(format!(
                    "{} at now - {age} s:\n    expected {expected}\n    hidane   {actual}",
                    read["name"].as_str().unwrap()
                ));
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[tokio::test]
async fn databases_without_a_limit() {
    let fixture = fixture();
    let recorded = &fixture["databases"];
    let count = recorded["count"].as_u64().unwrap();
    let (_, addr) = start().await;
    let names: Vec<String> = std::iter::once("(default)".to_owned())
        .chain((1..=count).map(|n| format!("db-{n}")))
        .collect();
    let mut writes = Map::new();
    for name in &names {
        let docs = format!("projects/databases/databases/{name}/documents");
        let write = json!({"writes": [{"update": {"name": format!("{docs}/c/d"), "fields": {"n": {"stringValue": name}}}}]});
        let (status, text) = send(
            addr,
            "POST",
            &format!("/v1/{docs}:commit"),
            Some(&write.to_string()),
        )
        .await;
        let key = if status == 200 {
            status.to_string()
        } else {
            format!("{status} {text}")
        };
        let seen = writes.get(&key).and_then(Json::as_u64).unwrap_or(0);
        writes.insert(key, json!(seen + 1));
    }
    assert_eq!(Json::Object(writes), recorded["writes"]);

    for read in recorded["reads"].as_array().unwrap() {
        let name = read["database"].as_str().unwrap();
        let (status, text) = send(
            addr,
            "GET",
            &format!("/v1/projects/databases/databases/{name}/documents/c/d"),
            None,
        )
        .await;
        let actual = json!({
            "database": name,
            "status": status,
            "response": masked(serde_json::from_str(&text).unwrap()),
        });
        assert_eq!(&actual, read);
    }
}
