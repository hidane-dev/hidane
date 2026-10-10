//! The Listen stream, message by message. The first answer to a target is the official
//! emulator's (`ADD`, the documents, `CURRENT`, a `NO_CHANGE` for all targets); later commits
//! send only what changed, as production does, where the official emulator resends every
//! watched result after `RESET` (docs/parity-exceptions.md). SDK-level equivalence with the
//! official emulator is checked by `tools/oracle/sdk_listen.mjs` (results/sdk-listen-*.json).

use std::time::Duration;

use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, ListenRequest, ListenResponse, StructuredQuery, Target, Value, Write,
    firestore_client::FirestoreClient,
    listen_request, listen_response,
    structured_query::{
        CollectionSelector, Direction, FieldFilter, FieldReference, Filter, Order, field_filter,
        filter,
    },
    target::{self, DocumentsTarget, QueryTarget, query_target},
    target_change::TargetChangeType,
    value::ValueType,
    write::Operation,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Streaming, transport::Channel};

type Client = FirestoreClient<Channel>;

const DB: &str = "projects/listen/databases/(default)";

fn doc_name(path: &str) -> String {
    format!("{DB}/documents/{path}")
}

async fn start() -> Client {
    start_with(hidane::Admin::default()).await.0
}

async fn start_with(admin: hidane::Admin) -> (Client, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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

async fn reset(addr: std::net::SocketAddr) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"POST /reset HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

fn set(path: &str, n: i64) -> Write {
    Write {
        operation: Some(Operation::Update(Document {
            name: doc_name(path),
            fields: [(
                "n".to_owned(),
                Value {
                    value_type: Some(ValueType::IntegerValue(n)),
                },
            )]
            .into(),
            ..Document::default()
        })),
        ..Write::default()
    }
}

fn delete(path: &str) -> Write {
    Write {
        operation: Some(Operation::Delete(doc_name(path))),
        ..Write::default()
    }
}

async fn commit(client: &mut Client, writes: Vec<Write>) {
    client
        .commit(CommitRequest {
            database: DB.to_owned(),
            writes,
            ..CommitRequest::default()
        })
        .await
        .unwrap();
}

/// `collection` where `n < 10`, so documents can leave it.
fn small_target(id: i32) -> Target {
    let mut target = query_target(id, "c", None);
    if let Some(target::TargetType::Query(QueryTarget {
        query_type: Some(query_target::QueryType::StructuredQuery(query)),
        ..
    })) = &mut target.target_type
    {
        query.r#where = Some(Filter {
            filter_type: Some(filter::FilterType::FieldFilter(FieldFilter {
                field: Some(FieldReference {
                    field_path: "n".to_owned(),
                }),
                op: field_filter::Operator::LessThan as i32,
                value: Some(Value {
                    value_type: Some(ValueType::IntegerValue(10)),
                }),
            })),
        });
    }
    target
}

fn query_target(id: i32, collection: &str, order_desc_limit: Option<i32>) -> Target {
    let mut query = StructuredQuery {
        from: vec![CollectionSelector {
            collection_id: collection.to_owned(),
            all_descendants: false,
        }],
        ..StructuredQuery::default()
    };
    if let Some(limit) = order_desc_limit {
        query.order_by = vec![Order {
            field: Some(FieldReference {
                field_path: "n".to_owned(),
            }),
            direction: Direction::Descending as i32,
        }];
        query.limit = Some(limit);
    }
    Target {
        target_id: id,
        target_type: Some(target::TargetType::Query(QueryTarget {
            parent: format!("{DB}/documents"),
            query_type: Some(query_target::QueryType::StructuredQuery(query)),
        })),
        ..Target::default()
    }
}

fn documents_target(id: i32, paths: &[&str]) -> Target {
    Target {
        target_id: id,
        target_type: Some(target::TargetType::Documents(DocumentsTarget {
            documents: paths.iter().map(|p| doc_name(p)).collect(),
        })),
        ..Target::default()
    }
}

struct Stream {
    requests: Option<mpsc::Sender<ListenRequest>>,
    responses: Streaming<ListenResponse>,
}

impl Stream {
    async fn open(client: &mut Client) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let responses = client
            .listen(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner();
        Self {
            requests: Some(tx),
            responses,
        }
    }

    async fn add(&self, target: Target) {
        self.send(listen_request::TargetChange::AddTarget(target))
            .await;
    }

    async fn remove(&self, id: i32) {
        self.send(listen_request::TargetChange::RemoveTarget(id))
            .await;
    }

    async fn send(&self, change: listen_request::TargetChange) {
        self.requests
            .as_ref()
            .unwrap()
            .send(ListenRequest {
                database: DB.to_owned(),
                target_change: Some(change),
                ..ListenRequest::default()
            })
            .await
            .unwrap();
    }

    /// The responses until nothing arrives for 300 ms, in a compact form.
    async fn drain(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(next) =
            tokio::time::timeout(Duration::from_millis(300), self.responses.message()).await
        {
            match next {
                Ok(Some(response)) => out.push(describe(&response)),
                Ok(None) => {
                    out.push("end".to_owned());
                    break;
                }
                Err(status) => {
                    out.push(format!("error {:?} {}", status.code(), status.message()));
                    break;
                }
            }
        }
        out
    }
}

fn relative(name: &str) -> &str {
    name.strip_prefix(&format!("{DB}/documents/"))
        .unwrap_or(name)
}

fn describe(response: &ListenResponse) -> String {
    match response.response_type.as_ref().unwrap() {
        listen_response::ResponseType::TargetChange(change) => {
            let kind = TargetChangeType::try_from(change.target_change_type)
                .unwrap()
                .as_str_name();
            let mut out = format!("{kind} {:?}", change.target_ids);
            if !change.resume_token.is_empty() {
                out.push_str(" token");
            }
            if change.read_time.is_some() {
                out.push_str(" read_time");
            }
            if let Some(cause) = &change.cause {
                out.push_str(&format!(" cause {} {}", cause.code, cause.message));
            }
            out
        }
        listen_response::ResponseType::DocumentChange(change) => {
            let doc = change.document.as_ref().unwrap();
            let n = match doc.fields.get("n").and_then(|v| v.value_type.as_ref()) {
                Some(ValueType::IntegerValue(n)) => *n,
                _ => -1,
            };
            assert!(doc.update_time.is_some(), "the SDKs need update_time");
            if change.target_ids.is_empty() {
                format!(
                    "left {}={n} {:?}",
                    relative(&doc.name),
                    change.removed_target_ids
                )
            } else {
                format!("change {}={n} {:?}", relative(&doc.name), change.target_ids)
            }
        }
        listen_response::ResponseType::DocumentDelete(delete) => {
            assert!(delete.read_time.is_some());
            format!(
                "delete {} {:?}",
                relative(&delete.document),
                delete.removed_target_ids
            )
        }
        listen_response::ResponseType::DocumentRemove(remove) => {
            format!(
                "remove {} {:?}",
                relative(&remove.document),
                remove.removed_target_ids
            )
        }
        listen_response::ResponseType::Filter(filter) => format!("filter {filter:?}"),
    }
}

#[tokio::test]
async fn first_answers_follow_the_official_emulator() {
    let mut client = start().await;
    commit(&mut client, vec![set("c/a", 1), set("c/b", 2)]).await;
    let mut stream = Stream::open(&mut client).await;
    stream.add(query_target(1, "c", None)).await;
    assert_eq!(
        stream.drain().await,
        [
            "ADD [1]",
            "change c/a=1 [1]",
            "change c/b=2 [1]",
            "CURRENT [1] token read_time",
            "NO_CHANGE [] token read_time",
        ]
    );
    stream
        .add(documents_target(2, &["c/b", "c/missing", "c/b"]))
        .await;
    assert_eq!(
        stream.drain().await,
        [
            "ADD [2]",
            "change c/b=2 [2]",
            "delete c/missing [2]",
            "CURRENT [2] token read_time",
            "NO_CHANGE [] token read_time",
        ]
    );
    // An empty collection still gets a consistent snapshot.
    stream.add(query_target(3, "empty", None)).await;
    assert_eq!(
        stream.drain().await,
        [
            "ADD [3]",
            "CURRENT [3] token read_time",
            "NO_CHANGE [] token read_time"
        ]
    );
}

#[tokio::test]
async fn later_commits_send_only_what_changed() {
    let mut client = start().await;
    commit(&mut client, vec![set("c/a", 1), set("c/b", 2)]).await;
    let mut stream = Stream::open(&mut client).await;
    stream.add(query_target(1, "c", None)).await;
    stream.add(documents_target(2, &["c/a"])).await;
    stream.drain().await;

    commit(&mut client, vec![set("c/a", 5)]).await;
    assert_eq!(
        stream.drain().await,
        [
            "change c/a=5 [1]",
            "change c/a=5 [2]",
            "NO_CHANGE [] token read_time"
        ]
    );
    // Within a commit, changes come in document path order.
    commit(&mut client, vec![set("c/new", 3), delete("c/b")]).await;
    assert_eq!(
        stream.drain().await,
        [
            "delete c/b [1]",
            "change c/new=3 [1]",
            "NO_CHANGE [] token read_time"
        ]
    );
    // Writes elsewhere and writes that change nothing send nothing.
    commit(&mut client, vec![set("d/x", 1), set("c/a", 5)]).await;
    assert!(stream.drain().await.is_empty());

    stream.remove(1).await;
    assert_eq!(stream.drain().await, ["REMOVE [1]"]);
    commit(&mut client, vec![set("c/new", 4)]).await;
    assert!(stream.drain().await.is_empty());
    commit(&mut client, vec![delete("c/a")]).await;
    assert_eq!(
        stream.drain().await,
        ["delete c/a [2]", "NO_CHANGE [] token read_time"]
    );
}

#[tokio::test]
async fn documents_leaving_a_query_are_removed() {
    let mut client = start().await;
    commit(
        &mut client,
        vec![set("c/a", 1), set("c/b", 2), set("c/c", 3)],
    )
    .await;
    let mut stream = Stream::open(&mut client).await;
    // Top 2 by n: a limit query runs again when its collection changes.
    stream.add(query_target(1, "c", Some(2))).await;
    assert_eq!(
        stream.drain().await,
        [
            "ADD [1]",
            "change c/c=3 [1]",
            "change c/b=2 [1]",
            "CURRENT [1] token read_time",
            "NO_CHANGE [] token read_time",
        ]
    );
    commit(&mut client, vec![set("c/d", 9)]).await;
    assert_eq!(
        stream.drain().await,
        [
            "left c/b=2 [1]",
            "change c/d=9 [1]",
            "NO_CHANGE [] token read_time"
        ]
    );
    commit(&mut client, vec![delete("c/d")]).await;
    assert_eq!(
        stream.drain().await,
        [
            "delete c/d [1]",
            "change c/b=2 [1]",
            "NO_CHANGE [] token read_time"
        ]
    );
    // A change below the limit that does not reach it sends nothing.
    commit(&mut client, vec![set("c/a", 0)]).await;
    assert!(stream.drain().await.is_empty());
}

#[tokio::test]
async fn target_ids_bad_targets_and_half_close() {
    let mut client = start().await;
    let mut stream = Stream::open(&mut client).await;
    // ID 0 asks the server for one, counting from 1 like the official emulator.
    stream.add(query_target(0, "c", None)).await;
    stream.add(query_target(0, "c", None)).await;
    let ids: Vec<String> = stream
        .drain()
        .await
        .into_iter()
        .filter(|r| r.starts_with("ADD"))
        .collect();
    assert_eq!(ids, ["ADD [1]", "ADD [2]"]);

    // A bad target is refused on its own.
    stream.add(documents_target(7, &["c"])).await;
    assert_eq!(
        stream.drain().await,
        [format!(
            "REMOVE [7] cause 3 Document name \"{DB}/documents/c\" lacks \"/\" at index 47."
        )]
    );
    // Removing a target that is not there is ignored.
    stream.remove(42).await;
    assert!(stream.drain().await.is_empty());

    // Half-closing ends the stream cleanly.
    stream.requests = None;
    assert_eq!(stream.drain().await, ["end"]);

    // A target ID that is in use ends the stream (the official emulator: UNKNOWN, no message).
    let mut stream = Stream::open(&mut client).await;
    stream.add(query_target(5, "c", None)).await;
    stream.add(query_target(5, "c", None)).await;
    let responses = stream.drain().await;
    assert_eq!(
        responses.last().unwrap(),
        &format!(
            "error {:?} Target ID 5 is already in use.",
            Code::InvalidArgument
        )
    );
}

#[tokio::test]
async fn resume_tokens_carry_the_read_time() {
    let mut client = start().await;
    commit(&mut client, vec![set("c/a", 1)]).await;
    let mut stream = Stream::open(&mut client).await;
    stream.add(query_target(1, "c", None)).await;
    let mut token = None;
    let mut read_time = None;
    for _ in 0..4 {
        let response = stream.responses.message().await.unwrap().unwrap();
        if let Some(listen_response::ResponseType::TargetChange(change)) = response.response_type
            && change.target_change_type == TargetChangeType::Current as i32
        {
            token = Some(change.resume_token);
            read_time = change.read_time;
        }
    }
    // The official encoding: field 1 { field 1: read time in microseconds }.
    let token = token.unwrap();
    let read_time = read_time.unwrap();
    let micros = read_time.seconds * 1_000_000 + i64::from(read_time.nanos / 1000);
    let mut expected = vec![0x08];
    let mut rest = u64::try_from(micros).unwrap();
    while rest >= 0x80 {
        expected.push(u8::try_from(rest & 0x7f).unwrap() | 0x80);
        rest >>= 7;
    }
    expected.push(u8::try_from(rest).unwrap());
    let mut wrapped = vec![0x0a, u8::try_from(expected.len()).unwrap()];
    wrapped.extend(expected);
    assert_eq!(token, wrapped);

    // Resuming with the token of the current state sends nothing but the closing messages.
    let mut resumed = query_target(2, "c", None);
    resumed.resume_type = Some(target::ResumeType::ResumeToken(token));
    stream.add(resumed).await;
    assert_eq!(
        stream.drain().await,
        [
            "ADD [2]",
            "CURRENT [2] token read_time",
            "NO_CHANGE [] token read_time"
        ]
    );
}

/// The token and read time of the snapshot a new target gets.
async fn current_token(stream: &mut Stream) -> (Vec<u8>, prost_types::Timestamp) {
    loop {
        let response = stream.responses.message().await.unwrap().unwrap();
        if let Some(listen_response::ResponseType::TargetChange(change)) = response.response_type
            && change.target_change_type == TargetChangeType::NoChange as i32
        {
            return (change.resume_token, change.read_time.unwrap());
        }
    }
}

#[tokio::test]
async fn a_resumed_target_gets_only_what_changed() {
    let mut client = start().await;
    commit(
        &mut client,
        vec![
            set("c/a", 1),
            set("c/b", 2),
            set("c/e", 3),
            set("c/same", 4),
        ],
    )
    .await;
    let mut stream = Stream::open(&mut client).await;
    stream.add(small_target(1)).await;
    let (token, read_time) = current_token(&mut stream).await;
    drop(stream);

    // While disconnected: a change, a deletion, a new document, one that leaves the query.
    commit(&mut client, vec![set("c/a", 5)]).await;
    commit(
        &mut client,
        vec![delete("c/b"), set("c/d", 6), set("c/e", 50)],
    )
    .await;

    for resume in [
        target::ResumeType::ResumeToken(token),
        target::ResumeType::ReadTime(read_time),
    ] {
        let mut stream = Stream::open(&mut client).await;
        let mut target = small_target(1);
        target.resume_type = Some(resume);
        stream.add(target).await;
        assert_eq!(
            stream.drain().await,
            [
                "ADD [1]",
                "change c/a=5 [1]",
                "change c/d=6 [1]",
                "delete c/b [1]",
                "left c/e=50 [1]",
                "CURRENT [1] token read_time",
                "NO_CHANGE [] token read_time",
            ]
        );
    }
}

#[tokio::test]
async fn a_target_resumed_from_beyond_the_kept_versions_gets_a_count() {
    let store = hidane_core::store::MemoryStore::new().with_retention(Duration::from_millis(200));
    let (mut client, _) = start_with(hidane::Admin::new(std::sync::Arc::new(store))).await;
    commit(
        &mut client,
        vec![set("c/a", 1), set("c/b", 2), set("c/c", 3)],
    )
    .await;
    let mut stream = Stream::open(&mut client).await;
    stream.add(small_target(1)).await;
    let (token, _) = current_token(&mut stream).await;
    drop(stream);

    tokio::time::sleep(Duration::from_millis(300)).await;
    commit(&mut client, vec![set("c/a", 7), delete("c/b")]).await;
    let mut stream = Stream::open(&mut client).await;
    let mut target = small_target(1);
    target.resume_type = Some(target::ResumeType::ResumeToken(token));
    stream.add(target).await;
    let responses = stream.drain().await;
    assert_eq!(responses[..2], ["ADD [1]", "change c/a=7 [1]"]);
    assert!(
        responses[2].starts_with("filter ExistenceFilter { target_id: 1, count: 2"),
        "{responses:?}"
    );
    assert_eq!(
        responses[3..],
        [
            "CURRENT [1] token read_time",
            "NO_CHANGE [] token read_time"
        ]
    );
}

#[tokio::test]
async fn untrusted_tokens_start_over() {
    let (mut client, addr) = start_with(hidane::Admin::default()).await;
    commit(&mut client, vec![set("c/a", 1)]).await;
    let mut stream = Stream::open(&mut client).await;
    stream.add(small_target(1)).await;
    let (token, _) = current_token(&mut stream).await;
    drop(stream);

    let started_over = [
        "ADD [1]",
        "RESET [1] token",
        "change c/a=1 [1]",
        "CURRENT [1] token read_time",
        "NO_CHANGE [] token read_time",
    ];
    // Not a token of ours, or from before this process started.
    for bad in [b"bogus".to_vec(), vec![0x0a, 0x02, 0x08, 0x01]] {
        let mut stream = Stream::open(&mut client).await;
        let mut target = small_target(1);
        target.resume_type = Some(target::ResumeType::ResumeToken(bad));
        stream.add(target).await;
        assert_eq!(stream.drain().await, started_over);
    }

    // From before a reset: the documents the client has may be gone without a trace.
    reset(addr).await;
    commit(&mut client, vec![set("c/a", 1)]).await;
    let mut stream = Stream::open(&mut client).await;
    let mut target = small_target(1);
    target.resume_type = Some(target::ResumeType::ResumeToken(token));
    stream.add(target).await;
    assert_eq!(stream.drain().await, started_over);
}
