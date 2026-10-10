//! Clearing data: `POST /reset`, `DELETE /emulator/v1/projects/{p}/databases/{d}/documents`
//! (rules-unit-testing's `clearFirestore()`, the Emulator UI's "Clear all data") and
//! `DELETE …/documents/{path}` (the Emulator UI's recursive delete). Scope, status codes and
//! bodies follow the official emulator v1.22.0; attached listeners are told, which the official
//! emulator does not do on a reset (docs/parity-exceptions.md).

use std::{net::SocketAddr, time::Duration};

use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, GetDocumentRequest, ListenRequest, ListenResponse, StructuredQuery,
    Target, Write,
    firestore_client::FirestoreClient,
    listen_request, listen_response,
    structured_query::CollectionSelector,
    target::{self, DocumentsTarget, QueryTarget, query_target},
    target_change::TargetChangeType,
    write::Operation,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Streaming, transport::Channel};

type Client = FirestoreClient<Channel>;

fn db(project: &str) -> String {
    format!("projects/{project}/databases/(default)")
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

/// Status line, content type and body of an HTTP request without a body.
async fn http(addr: SocketAddr, method: &str, path: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    let content_type = head
        .lines()
        .find_map(|l| l.strip_prefix("content-type: "))
        .unwrap_or_default()
        .to_owned();
    (status, content_type, body.to_owned())
}

async fn seed(client: &mut Client, project: &str, paths: &[&str]) {
    client
        .commit(CommitRequest {
            database: db(project),
            writes: paths
                .iter()
                .map(|p| Write {
                    operation: Some(Operation::Update(Document {
                        name: format!("{}/documents/{p}", db(project)),
                        ..Document::default()
                    })),
                    ..Write::default()
                })
                .collect(),
            ..CommitRequest::default()
        })
        .await
        .unwrap();
}

async fn exists(client: &mut Client, project: &str, path: &str) -> bool {
    match client
        .get_document(GetDocumentRequest {
            name: format!("{}/documents/{path}", db(project)),
            ..GetDocumentRequest::default()
        })
        .await
    {
        Ok(_) => true,
        Err(status) if status.code() == Code::NotFound => false,
        Err(status) => panic!("{status}"),
    }
}

async fn present(client: &mut Client, project: &str, paths: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        if exists(client, project, path).await {
            out.push((*path).to_owned());
        }
    }
    out
}

const EMPTY: (u16, &str, &str) = (200, "application/json", "{\n}\n");

#[tokio::test]
async fn deleting_a_database_or_a_path() {
    let (mut client, addr) = start().await;
    let all = ["c/a", "c/a/sub/x", "c/ab", "c/b", "d/z"];
    seed(&mut client, "p", &all).await;
    seed(&mut client, "other", &all).await;

    // A document and everything below it, not its neighbours.
    let r = http(
        addr,
        "DELETE",
        "/emulator/v1/projects/p/databases/(default)/documents/c/a",
    )
    .await;
    assert_eq!((r.0, r.1.as_str(), r.2.as_str()), EMPTY);
    assert_eq!(
        present(&mut client, "p", &all).await,
        ["c/ab", "c/b", "d/z"]
    );
    // A collection and everything below it.
    let r = http(
        addr,
        "DELETE",
        "/emulator/v1/projects/p/databases/(default)/documents/c",
    )
    .await;
    assert_eq!((r.0, r.1.as_str(), r.2.as_str()), EMPTY);
    assert_eq!(present(&mut client, "p", &all).await, ["d/z"]);
    // Nothing there: still fine.
    let r = http(
        addr,
        "DELETE",
        "/emulator/v1/projects/p/databases/(default)/documents/c/zzz",
    )
    .await;
    assert_eq!((r.0, r.1.as_str(), r.2.as_str()), EMPTY);
    // A reserved ID is refused with the official error body.
    let r = http(
        addr,
        "DELETE",
        "/emulator/v1/projects/p/databases/(default)/documents/c/__x__",
    )
    .await;
    assert_eq!(
        (r.0, r.1.as_str(), r.2.as_str()),
        (
            400,
            "application/json",
            r#"{"error":{"code":400,"message":"Resource id \"__x__\" is invalid because it is reserved.","status":"INVALID_ARGUMENT"}}"#
        )
    );
    // The whole database (with or without a trailing slash), not other projects.
    let r = http(
        addr,
        "DELETE",
        "/emulator/v1/projects/p/databases/(default)/documents/",
    )
    .await;
    assert_eq!((r.0, r.1.as_str(), r.2.as_str()), EMPTY);
    assert!(present(&mut client, "p", &all).await.is_empty());
    assert_eq!(present(&mut client, "other", &all).await, all);
    let r = http(
        addr,
        "DELETE",
        "/emulator/v1/projects/other/databases/(default)/documents",
    )
    .await;
    assert_eq!((r.0, r.1.as_str(), r.2.as_str()), EMPTY);
    assert!(present(&mut client, "other", &all).await.is_empty());

    // Other methods and shapes are not found.
    for (method, path) in [
        (
            "GET",
            "/emulator/v1/projects/p/databases/(default)/documents",
        ),
        (
            "POST",
            "/emulator/v1/projects/p/databases/(default)/documents",
        ),
        ("DELETE", "/emulator/v1/projects/p/databases/documents"),
    ] {
        assert_eq!(http(addr, method, path).await.0, 404, "{method} {path}");
    }
}

struct Listener {
    _requests: mpsc::Sender<ListenRequest>,
    responses: Streaming<ListenResponse>,
}

/// A listener on collection `c` (target 1) and on `c/a` (target 2) of project `project`.
async fn listen(client: &mut Client, project: &str) -> Listener {
    let (tx, rx) = mpsc::channel(16);
    let database = db(project);
    let documents = format!("{database}/documents");
    for target in [
        Target {
            target_id: 1,
            target_type: Some(target::TargetType::Query(QueryTarget {
                parent: documents.clone(),
                query_type: Some(query_target::QueryType::StructuredQuery(StructuredQuery {
                    from: vec![CollectionSelector {
                        collection_id: "c".to_owned(),
                        all_descendants: false,
                    }],
                    ..StructuredQuery::default()
                })),
            })),
            ..Target::default()
        },
        Target {
            target_id: 2,
            target_type: Some(target::TargetType::Documents(DocumentsTarget {
                documents: vec![format!("{documents}/c/a")],
            })),
            ..Target::default()
        },
    ] {
        tx.send(ListenRequest {
            database: database.clone(),
            target_change: Some(listen_request::TargetChange::AddTarget(target)),
            ..ListenRequest::default()
        })
        .await
        .unwrap();
    }
    let mut listener = Listener {
        responses: client
            .listen(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner(),
        _requests: tx,
    };
    listener.drain(&database).await;
    listener
}

impl Listener {
    async fn drain(&mut self, database: &str) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(Ok(Some(response))) =
            tokio::time::timeout(Duration::from_millis(300), self.responses.message()).await
        {
            out.push(match response.response_type.unwrap() {
                listen_response::ResponseType::TargetChange(change) => format!(
                    "{} {:?}",
                    TargetChangeType::try_from(change.target_change_type)
                        .unwrap()
                        .as_str_name(),
                    change.target_ids
                ),
                listen_response::ResponseType::DocumentDelete(delete) => format!(
                    "delete {} {:?}",
                    delete
                        .document
                        .strip_prefix(&format!("{database}/documents/"))
                        .unwrap(),
                    delete.removed_target_ids
                ),
                other => format!("{other:?}"),
            });
        }
        out
    }
}

#[tokio::test]
async fn listeners_see_cleared_documents_go() {
    let (mut client, addr) = start().await;
    let snapshot = "NO_CHANGE []";
    for (clear, project) in [
        (
            "/emulator/v1/projects/db/databases/(default)/documents",
            "db",
        ),
        (
            "/emulator/v1/projects/path/databases/(default)/documents/c",
            "path",
        ),
        ("/reset", "reset"),
    ] {
        seed(&mut client, project, &["c/a", "c/b"]).await;
        let mut listener = listen(&mut client, project).await;
        let method = if clear == "/reset" { "POST" } else { "DELETE" };
        assert_eq!(http(addr, method, clear).await.0, 200);
        let database = db(project);
        let mut seen = listener.drain(&database).await;
        let last = seen.pop();
        seen.sort();
        assert_eq!(
            seen,
            ["delete c/a [1]", "delete c/a [2]", "delete c/b [1]"],
            "{clear}"
        );
        assert_eq!(last.as_deref(), Some(snapshot), "{clear}");
        // Writes after the clear arrive as usual.
        seed(&mut client, project, &["c/b"]).await;
        assert_eq!(
            listener.drain(&database).await.last().map(String::as_str),
            Some(snapshot)
        );
    }
}

#[tokio::test]
async fn resume_tokens_from_before_a_clear_start_over() {
    let (mut client, addr) = start().await;
    seed(&mut client, "t", &["c/a"]).await;
    let database = db("t");
    let (tx, rx) = mpsc::channel(16);
    let mut target = Target {
        target_id: 1,
        target_type: Some(target::TargetType::Documents(DocumentsTarget {
            documents: vec![format!("{database}/documents/c/a")],
        })),
        ..Target::default()
    };
    tx.send(ListenRequest {
        database: database.clone(),
        target_change: Some(listen_request::TargetChange::AddTarget(target.clone())),
        ..ListenRequest::default()
    })
    .await
    .unwrap();
    let mut responses = client
        .listen(ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    let token = loop {
        if let Some(listen_response::ResponseType::TargetChange(change)) =
            responses.message().await.unwrap().unwrap().response_type
            && change.target_change_type == TargetChangeType::Current as i32
        {
            break change.resume_token;
        }
    };
    drop(tx);
    http(
        addr,
        "DELETE",
        "/emulator/v1/projects/t/databases/(default)/documents",
    )
    .await;
    seed(&mut client, "t", &["c/a"]).await;

    let (tx, rx) = mpsc::channel(16);
    target.resume_type = Some(target::ResumeType::ResumeToken(token));
    tx.send(ListenRequest {
        database,
        target_change: Some(listen_request::TargetChange::AddTarget(target)),
        ..ListenRequest::default()
    })
    .await
    .unwrap();
    let mut responses = client
        .listen(ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    let mut kinds = Vec::new();
    for _ in 0..2 {
        if let Some(listen_response::ResponseType::TargetChange(change)) =
            responses.message().await.unwrap().unwrap().response_type
        {
            kinds.push(change.target_change_type);
        }
    }
    assert_eq!(
        kinds,
        [TargetChangeType::Add as i32, TargetChangeType::Reset as i32]
    );
}
