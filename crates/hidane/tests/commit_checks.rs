//! How a commit's writes are checked, against the official emulator's recordings
//! (`fixtures/commit_checks.json`, from `tools/oracle/commit_checks.py`).
//!
//! `order` (#202): commits with problems, in two writes or in one, over REST `:commit`, gRPC
//! `Commit`, REST `:batchWrite` and the Write stream, each in the project the recording used.
//! Every answer is compared: the code and message of the first error (a failed precondition on
//! its code: its message prints the official emulator's internal key, docs/parity-exceptions.md),
//! or the number of write results, or `:batchWrite`'s statuses.
//!
//! `other_database` (#203): writes to a document of another database, which the official
//! emulator acknowledges and keeps in the request's database under the other database's name,
//! where no read but a batchGet of that name finds it and an export picks it up. hidane refuses
//! them (docs/parity-exceptions.md): the steps in `DIFFERS` are checked against that refusal and
//! what follows from it, the others against the official answer.

mod common;

use std::{net::SocketAddr, path::PathBuf};

use common::protojson;
use hidane_proto::google::firestore::v1::{WriteRequest, firestore_client::FirestoreClient};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Request, Status, transport::Channel};

type Client = FirestoreClient<Channel>;

/// Cases where hidane answers differently on purpose (docs/parity-exceptions.md): an array
/// union's operand that holds an array or a vector with NaN is refused when the transform is
/// applied; the official emulator stores it.
const KNOWN: &[&str] = &[
    "an array union of an array",
    "an array union of a vector with NaN",
];

/// Steps of `other_database` where hidane answers differently on purpose: it refuses a write to
/// a document of another database, so the reads that follow find nothing, and a batchGet
/// naming another database's document is refused too.
const DIFFERS: &[&str] = &[
    "REST :commit writes a document of another database",
    "gRPC Commit writes a document of another database",
    "REST :batchWrite writes a document of another database",
    "the Write stream writes a document of another database",
    "a transaction's commit writes a document of another database",
    "a commit writes a document of its database and one of another",
    "a commit writes a document of another project",
    "GET projects/other-db/databases/(default)/documents/c/own",
    "a query of the request's database",
    "a batchGet in the request's database of the documents it wrote",
    "an export of the request's database, imported into a database of the other name",
];

async fn start() -> (SocketAddr, Client) {
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
    (addr, FirestoreClient::new(channel))
}

/// `{repeat:<c>:<n>}` stands for `<c>` repeated `<n>` times.
fn expand(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("{repeat:") {
        out.push_str(&rest[..at]);
        let token = &rest[at + "{repeat:".len()..];
        let end = token.find('}').unwrap();
        let (c, n) = token[..end].rsplit_once(':').unwrap();
        out.push_str(&c.repeat(n.parse().unwrap()));
        rest = &token[end + 1..];
    }
    out.push_str(rest);
    out
}

fn expand_json(value: &Json) -> Json {
    serde_json::from_str(&expand(&value.to_string())).unwrap()
}

fn code_name(code: Code) -> String {
    let mut out = String::new();
    for (i, c) in format!("{code:?}").chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

fn grpc_error(status: &Status) -> Json {
    json!({"code": code_name(status.code()), "message": status.message()})
}

/// An HTTP request to hidane; the status and the body as JSON (`null` if it is not).
async fn http(addr: SocketAddr, method: &str, path: &str, body: Option<&Json>) -> (u16, Json) {
    let body = body.map(Json::to_string).unwrap_or_default();
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
    (status, serde_json::from_str(body).unwrap_or(Json::Null))
}

/// A REST answer as the recording summarises it.
fn rest_outcome(status: u16, body: &Json) -> Json {
    if status != 200 {
        let error = &body["error"];
        return json!({"status": status, "code": error["status"], "message": error["message"]});
    }
    if let Some(statuses) = body["status"].as_array() {
        let statuses: Vec<Json> = statuses
            .iter()
            .map(|s| {
                let code = i32::try_from(s["code"].as_i64().unwrap_or(0)).unwrap();
                json!({
                    "code": code_name(Code::from_i32(code)),
                    "message": s["message"].as_str().unwrap_or(""),
                })
            })
            .collect();
        return json!({"status": status, "code": "OK", "statuses": statuses});
    }
    let results = body["writeResults"].as_array().map_or(0, Vec::len);
    json!({"status": status, "code": "OK", "results": results})
}

async fn rest_write(addr: SocketAddr, database: &str, method: &str, body: &Json) -> Json {
    let path = format!("/v1/{database}/documents:{method}");
    let (status, body) = http(addr, "POST", &path, Some(body)).await;
    rest_outcome(status, &body)
}

async fn send(
    addr: SocketAddr,
    client: &mut Client,
    transport: &str,
    database: &str,
    writes: &Json,
) -> Json {
    let body = json!({"writes": writes});
    let batch = || protojson::commit_request(&format!("{database}/documents:commit"), &body);
    match transport {
        "commit" => rest_write(addr, database, "commit", &body).await,
        "batch_write" => rest_write(addr, database, "batchWrite", &body).await,
        "grpc_commit" => match client.commit(batch()).await {
            Ok(response) => {
                json!({"code": "OK", "results": response.into_inner().write_results.len()})
            }
            Err(status) => grpc_error(&status),
        },
        "write_stream" => {
            let (tx, rx) = mpsc::channel(4);
            tx.send(WriteRequest {
                database: database.to_owned(),
                ..WriteRequest::default()
            })
            .await
            .unwrap();
            let mut stream = client
                .write(Request::new(ReceiverStream::new(rx)))
                .await
                .unwrap()
                .into_inner();
            let handshake = stream.message().await.unwrap().unwrap();
            tx.send(WriteRequest {
                stream_token: handshake.stream_token,
                writes: batch().writes,
                ..WriteRequest::default()
            })
            .await
            .unwrap();
            match stream.message().await {
                Ok(Some(response)) => {
                    json!({"code": "OK", "results": response.write_results.len()})
                }
                Ok(None) => json!({"code": "OK", "results": 0}),
                Err(status) => grpc_error(&status),
            }
        }
        other => panic!("unknown transport {other}"),
    }
}

/// Whether hidane's answer is the official one: the same code and message, but only the code
/// of a failed precondition.
fn same(official: &Json, hidane: &Json) -> bool {
    let precondition = |code: &Json| matches!(code.as_str(), Some("NOT_FOUND" | "ALREADY_EXISTS"));
    let without_key = |outcome: &Json| {
        let mut outcome = outcome.clone();
        if precondition(&outcome["code"]) {
            outcome["message"] = Json::Null;
        }
        if let Some(statuses) = outcome["statuses"].as_array_mut() {
            for status in statuses.iter_mut().filter(|s| precondition(&s["code"])) {
                status["message"] = Json::Null;
            }
        }
        outcome
    };
    without_key(&expand_json(official)) == without_key(hidane)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_error_is_the_official_one() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/commit_checks.json")).unwrap();
    let cases = fixture["order"].as_array().unwrap();
    assert!(cases.len() > 1000, "only {} cases", cases.len());
    let (addr, mut client) = start().await;
    let mut differences = Vec::new();
    let mut known = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        for (transport, official) in case["outcomes"].as_object().unwrap() {
            let project = case["project"].as_str().unwrap();
            let database = format!(
                "projects/{project}-{}/databases/(default)",
                transport.replace('_', "-")
            );
            let text = case["writes"]
                .to_string()
                .replace("{base}", &format!("{database}/documents"));
            let writes: Json = serde_json::from_str(&expand(&text)).unwrap();
            let hidane = send(addr, &mut client, transport, &database, &writes).await;
            if KNOWN.contains(&name) {
                // Refused when the transform is applied, with the message a field would get
                // (`:batchWrite` reports it as the write's status).
                let code = hidane
                    .get("statuses")
                    .map_or(&hidane["code"], |statuses| &statuses[0]["code"]);
                assert_eq!(code, "INVALID_ARGUMENT", "{name} / {transport}");
                assert_eq!(official["code"], "OK", "{name} / {transport}");
                known += 1;
            } else if !same(official, &hidane) {
                differences.push(format!(
                    "{name} / {transport}:\n    official {official}\n    hidane   {hidane}"
                ));
            }
        }
    }
    assert_eq!(known, KNOWN.len() * 4, "every known difference is recorded");
    assert!(
        differences.is_empty(),
        "{} answers differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// A directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hidane-commit-checks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn query(addr: SocketAddr, database: &str) -> Json {
    let (status, rows) = http(
        addr,
        "POST",
        &format!("/v1/{database}/documents:runQuery"),
        Some(&json!({"structuredQuery": {"from": [{"collectionId": "c"}]}})),
    )
    .await;
    let documents: Vec<&Json> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row.get("document").map(|d| &d["name"]))
        .collect();
    json!({"status": status, "documents": documents})
}

/// Replays one step of `other_database`; returns hidane's answer as the recording summarises
/// the official one.
async fn replay(addr: SocketAddr, client: &mut Client, step: &Json, export: &TempDir) -> Json {
    let database = step["database"].as_str().unwrap_or_default();
    match step["op"].as_str().unwrap() {
        op @ ("commit" | "grpc_commit" | "batch_write" | "write_stream") => {
            send(addr, client, op, database, &step["writes"]).await
        }
        "transaction" => {
            let path = format!("/v1/{database}/documents:beginTransaction");
            let (status, body) = http(addr, "POST", &path, Some(&json!({}))).await;
            assert_eq!(status, 200, "{body}");
            let body = json!({"transaction": body["transaction"], "writes": step["writes"]});
            rest_write(addr, database, "commit", &body).await
        }
        "get" => {
            let path = format!("/v1/{}", step["document"].as_str().unwrap());
            json!({"status": http(addr, "GET", &path, None).await.0})
        }
        "query" => query(addr, database).await,
        "batch_get" => {
            let path = format!("/v1/{database}/documents:batchGet");
            let body = json!({"documents": step["documents"]});
            let (status, rows) = http(addr, "POST", &path, Some(&body)).await;
            if status != 200 {
                return rest_outcome(status, &rows);
            }
            let rows = rows.as_array().unwrap();
            let names = |key: &str| -> Vec<Json> {
                rows.iter()
                    .filter_map(|r| r.get(key))
                    .map(|v| v.get("name").unwrap_or(v).clone())
                    .collect()
            };
            json!({"status": status, "found": names("found"), "missing": names("missing")})
        }
        "export_import" => {
            let project = |name: &str| name.split('/').nth(1).unwrap().to_owned();
            let directory = export.0.to_str().unwrap();
            let (status, body) = http(
                addr,
                "POST",
                &format!("/emulator/v1/projects/{}:export", project(database)),
                Some(&json!({"database": database, "export_directory": directory, "export_name": "own"})),
            )
            .await;
            assert_eq!(status, 200, "{body}");
            let import = step["import"].as_str().unwrap();
            let (status, body) = http(
                addr,
                "POST",
                &format!("/emulator/v1/projects/{}:import", project(import)),
                Some(&json!({
                    "database": import,
                    "export_directory": format!("{directory}/own/own.overall_export_metadata"),
                })),
            )
            .await;
            assert_eq!(status, 200, "{body}");
            query(addr, import).await
        }
        other => panic!("unknown step {other}"),
    }
}

/// What hidane answers instead where it differs on purpose.
fn refused(step: &Json, database: &str) -> Json {
    let other = |name: &&Json| !name.as_str().unwrap().starts_with(&format!("{database}/"));
    match step["op"].as_str().unwrap() {
        "get" => json!({"status": 404}),
        "query" | "export_import" => json!({"status": 200, "documents": []}),
        "batch_get" => {
            let name = step["documents"]
                .as_array()
                .unwrap()
                .iter()
                .find(other)
                .unwrap();
            json!({
                "status": 400,
                "code": "INVALID_ARGUMENT",
                "message": format!("Document {name} is not in database \"{database}\"."),
            })
        }
        op => {
            let name = step["writes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| &w["update"]["name"])
                .find(other)
                .unwrap();
            let message = format!("Document {name} is not in database \"{database}\".");
            if matches!(op, "grpc_commit" | "write_stream") {
                json!({"code": "INVALID_ARGUMENT", "message": message})
            } else {
                json!({"status": 400, "code": "INVALID_ARGUMENT", "message": message})
            }
        }
    }
}

#[tokio::test]
async fn writes_to_another_database_are_refused() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/commit_checks.json")).unwrap();
    let steps = fixture["other_database"].as_array().unwrap();
    let (addr, mut client) = start().await;
    let export = TempDir::new();
    let mut differs = 0;
    for step in steps {
        let name = step["name"].as_str().unwrap();
        let hidane = replay(addr, &mut client, step, &export).await;
        let official = &step["outcome"];
        if DIFFERS.contains(&name) {
            differs += 1;
            assert_ne!(&hidane, official, "{name}: no longer differs");
            let database = step["database"].as_str().unwrap_or_default();
            assert_eq!(hidane, refused(step, database), "{name}");
        } else {
            assert_eq!(&hidane, official, "{name}");
        }
    }
    assert_eq!(differs, DIFFERS.len(), "every listed step is recorded");
}
