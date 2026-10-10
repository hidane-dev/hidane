//! Transactions against the official emulator's recordings (`fixtures/transactions.json`, from
//! `tools/oracle/transactions.py`). The fixture keeps each scenario's steps next to the official
//! outcome; this test runs the same steps against hidane over gRPC and compares status, message,
//! elapsed time (rounded to 0.5 s, so lock waits show) and result. Lock waits are real, so every
//! scenario gets its own server and they all run at once. Scenarios marked `slow` (over a minute
//! of waiting) are covered with a paused clock by the unit tests in `src/firestore/transactions`.

use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hidane_proto::google::firestore::v1::{
    BatchGetDocumentsRequest, BatchWriteRequest, BeginTransactionRequest, CommitRequest, Document,
    DocumentMask, GetDocumentRequest, ListDocumentsRequest, Precondition, RollbackRequest,
    RunQueryRequest, StructuredQuery, TransactionOptions, Value, Write,
    batch_get_documents_request, batch_get_documents_response,
    document_transform::{FieldTransform, field_transform::TransformType},
    firestore_client::FirestoreClient,
    get_document_request, list_documents_request,
    precondition::ConditionType,
    run_query_request,
    structured_query::CollectionSelector,
    transaction_options,
    value::ValueType,
    write::Operation,
};
use prost_types::Timestamp;
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tonic::{Code, Request, Status, transport::Channel};

type Client = FirestoreClient<Channel>;

fn timestamp(rfc3339: &str) -> Timestamp {
    let t = chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap();
    Timestamp {
        seconds: t.timestamp(),
        nanos: i32::try_from(t.timestamp_subsec_nanos()).unwrap(),
    }
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

/// The server SDKs' credentials; BatchWrite needs them.
fn owner<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", "Bearer owner".parse().unwrap());
    request
}

fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "OK",
        Code::Cancelled => "CANCELLED",
        Code::Unknown => "UNKNOWN",
        Code::InvalidArgument => "INVALID_ARGUMENT",
        Code::DeadlineExceeded => "DEADLINE_EXCEEDED",
        Code::NotFound => "NOT_FOUND",
        Code::AlreadyExists => "ALREADY_EXISTS",
        Code::PermissionDenied => "PERMISSION_DENIED",
        Code::ResourceExhausted => "RESOURCE_EXHAUSTED",
        Code::FailedPrecondition => "FAILED_PRECONDITION",
        Code::Aborted => "ABORTED",
        Code::OutOfRange => "OUT_OF_RANGE",
        Code::Unimplemented => "UNIMPLEMENTED",
        Code::Internal => "INTERNAL",
        Code::Unavailable => "UNAVAILABLE",
        Code::DataLoss => "DATA_LOSS",
        Code::Unauthenticated => "UNAUTHENTICATED",
    }
}

/// One scenario's context: where to send requests and the transactions opened so far.
#[derive(Clone)]
struct Context {
    client: Client,
    addr: SocketAddr,
    database: String,
    transactions: HashMap<String, Vec<u8>>,
}

impl Context {
    fn base(&self) -> String {
        format!("{}/documents", self.database)
    }

    fn name(&self, relative: &str) -> String {
        format!("{}/{relative}", self.base())
    }

    fn relative(&self, name: &str) -> String {
        name.strip_prefix(&format!("{}/", self.base()))
            .unwrap_or(name)
            .to_owned()
    }

    fn transaction(&self, step: &Json) -> Vec<u8> {
        if let Some(raw) = step["raw"].as_str() {
            return STANDARD.decode(raw).unwrap();
        }
        step["txn"]
            .as_str()
            .map(|t| self.transactions[t].clone())
            .unwrap_or_default()
    }

    /// `options` in REST JSON, with `{T}` standing for transaction T's ID.
    fn options(&self, options: &Json) -> TransactionOptions {
        let mode = if let Some(read_only) = options.get("readOnly") {
            transaction_options::Mode::ReadOnly(transaction_options::ReadOnly {
                consistency_selector: read_only["readTime"].as_str().map(|ts| {
                    transaction_options::read_only::ConsistencySelector::ReadTime(timestamp(ts))
                }),
            })
        } else {
            let retry = options["readWrite"]["retryTransaction"]
                .as_str()
                .map(
                    |r| match r.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
                        Some(name) => self.transactions[name].clone(),
                        None => STANDARD.decode(r).unwrap(),
                    },
                )
                .unwrap_or_default();
            transaction_options::Mode::ReadWrite(transaction_options::ReadWrite {
                retry_transaction: retry,
                concurrency_mode: match options["readWrite"]["concurrencyMode"].as_str() {
                    Some("OPTIMISTIC") => transaction_options::ConcurrencyMode::Optimistic,
                    Some("PESSIMISTIC") => transaction_options::ConcurrencyMode::Pessimistic,
                    _ => transaction_options::ConcurrencyMode::Unspecified,
                } as i32,
            })
        };
        TransactionOptions { mode: Some(mode) }
    }

    fn write(&self, w: &Json) -> Write {
        let operation = if let Some(name) = w["delete"].as_str() {
            Operation::Delete(self.name(name))
        } else if let Some(name) = w["verify"].as_str() {
            Operation::Verify(self.name(name))
        } else {
            let mut fields = std::collections::BTreeMap::new();
            if let Some(n) = w["n"].as_i64() {
                fields.insert(
                    "n".to_owned(),
                    Value {
                        value_type: Some(ValueType::IntegerValue(n)),
                    },
                );
            }
            if w["reserved"] == true {
                fields.insert(
                    "__x__".to_owned(),
                    Value {
                        value_type: Some(ValueType::NullValue(0)),
                    },
                );
            }
            Operation::Update(Document {
                name: self.name(w["update"].as_str().unwrap()),
                fields,
                ..Document::default()
            })
        };
        Write {
            operation: Some(operation),
            current_document: w["exists"].as_bool().map(|exists| Precondition {
                condition_type: Some(ConditionType::Exists(exists)),
            }),
            update_mask: w["mask"].as_array().map(|paths| DocumentMask {
                field_paths: paths
                    .iter()
                    .map(|p| p.as_str().unwrap().to_owned())
                    .collect(),
            }),
            update_transforms: w["increment"]
                .as_str()
                .map(|field| {
                    vec![FieldTransform {
                        field_path: field.to_owned(),
                        transform_type: Some(TransformType::Increment(Value {
                            value_type: Some(ValueType::IntegerValue(1)),
                        })),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    fn document(&self, doc: &Document) -> Json {
        let n = match doc.fields.get("n").and_then(|v| v.value_type.as_ref()) {
            Some(ValueType::IntegerValue(n)) => json!(n),
            _ => Json::Null,
        };
        json!({"name": self.relative(&doc.name), "n": n})
    }

    /// Runs one step; returns its result and the transactions it opened.
    async fn execute(
        mut self,
        step: &Json,
    ) -> (Result<Option<Json>, Status>, Vec<(String, Vec<u8>)>) {
        let mut opened = Vec::new();
        let database = self.database.clone();
        let result = match step["op"].as_str().unwrap() {
            "begin" => {
                let options = step.get("options").map(|o| self.options(o));
                self.client
                    .begin_transaction(owner(BeginTransactionRequest {
                        database,
                        options,
                        ..BeginTransactionRequest::default()
                    }))
                    .await
                    .map(|r| {
                        let id = r.into_inner().transaction;
                        opened.push((step["as"].as_str().unwrap().to_owned(), id.clone()));
                        Some(json!({"transaction": STANDARD.encode(id)}))
                    })
            }
            "read" => {
                let consistency_selector = if let Some(new) = step.get("new") {
                    Some(
                        batch_get_documents_request::ConsistencySelector::NewTransaction(
                            self.options(new),
                        ),
                    )
                } else if step.get("txn").is_some() {
                    Some(
                        batch_get_documents_request::ConsistencySelector::Transaction(
                            self.transaction(step),
                        ),
                    )
                } else {
                    None
                };
                let request = BatchGetDocumentsRequest {
                    database,
                    documents: step["documents"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|d| self.name(d.as_str().unwrap()))
                        .collect(),
                    consistency_selector,
                    ..BatchGetDocumentsRequest::default()
                };
                match self.client.batch_get_documents(owner(request)).await {
                    Err(status) => Err(status),
                    Ok(stream) => {
                        let mut stream = stream.into_inner();
                        let mut out = Vec::new();
                        let mut failure = None;
                        loop {
                            match stream.message().await {
                                Ok(Some(r)) => {
                                    if !r.transaction.is_empty() {
                                        opened.push((
                                            step["as"].as_str().unwrap().to_owned(),
                                            r.transaction.clone(),
                                        ));
                                        out.push(
                                            json!({"transaction": STANDARD.encode(&r.transaction)}),
                                        );
                                    }
                                    match r.result {
                                        Some(batch_get_documents_response::Result::Found(doc)) => {
                                            out.push(json!({"found": self.document(&doc)}));
                                        }
                                        Some(batch_get_documents_response::Result::Missing(
                                            name,
                                        )) => out.push(json!({"missing": self.relative(&name)})),
                                        None => {}
                                    }
                                }
                                Ok(None) => break,
                                Err(status) => {
                                    failure = Some(status);
                                    break;
                                }
                            }
                        }
                        match failure {
                            Some(status) => Err(status),
                            None => Ok(Some(Json::Array(out))),
                        }
                    }
                }
            }
            "get" => {
                let request = GetDocumentRequest {
                    name: self.name(step["document"].as_str().unwrap()),
                    mask: None,
                    consistency_selector: step.get("txn").map(|_| {
                        get_document_request::ConsistencySelector::Transaction(
                            self.transaction(step),
                        )
                    }),
                    ..GetDocumentRequest::default()
                };
                self.client
                    .get_document(owner(request))
                    .await
                    .map(|r| Some(self.document(&r.into_inner())))
            }
            "list" => {
                let request = ListDocumentsRequest {
                    parent: self.base(),
                    collection_id: step["collection"].as_str().unwrap().to_owned(),
                    page_size: step["pageSize"].as_i64().map_or(0, |n| n as i32),
                    consistency_selector: step.get("txn").map(|_| {
                        list_documents_request::ConsistencySelector::Transaction(
                            self.transaction(step),
                        )
                    }),
                    ..ListDocumentsRequest::default()
                };
                self.client.list_documents(owner(request)).await.map(|r| {
                    Some(Json::Array(
                        r.into_inner()
                            .documents
                            .iter()
                            .map(|d| self.document(d))
                            .collect(),
                    ))
                })
            }
            "query" => {
                let consistency_selector = if let Some(new) = step.get("new") {
                    Some(run_query_request::ConsistencySelector::NewTransaction(
                        self.options(new),
                    ))
                } else if step.get("txn").is_some() {
                    Some(run_query_request::ConsistencySelector::Transaction(
                        self.transaction(step),
                    ))
                } else {
                    None
                };
                let request = RunQueryRequest {
                    parent: self.base(),
                    query_type: Some(run_query_request::QueryType::StructuredQuery(
                        StructuredQuery {
                            from: vec![CollectionSelector {
                                collection_id: step["collection"].as_str().unwrap().to_owned(),
                                all_descendants: step["group"] == true,
                            }],
                            ..StructuredQuery::default()
                        },
                    )),
                    consistency_selector,
                    ..RunQueryRequest::default()
                };
                match self.client.run_query(owner(request)).await {
                    Err(status) => Err(status),
                    Ok(stream) => {
                        let mut stream = stream.into_inner();
                        let mut out = Vec::new();
                        loop {
                            match stream.message().await {
                                Ok(Some(r)) => {
                                    if !r.transaction.is_empty() {
                                        opened.push((
                                            step["as"].as_str().unwrap().to_owned(),
                                            r.transaction.clone(),
                                        ));
                                        out.push(
                                            json!({"transaction": STANDARD.encode(&r.transaction)}),
                                        );
                                    }
                                    if let Some(doc) = &r.document {
                                        out.push(json!({"document": self.document(doc)}));
                                    }
                                }
                                Ok(None) => break Ok(Some(Json::Array(out))),
                                Err(status) => break Err(status),
                            }
                        }
                    }
                }
            }
            "commit" => {
                let request = CommitRequest {
                    database,
                    writes: writes(&self, step),
                    transaction: self.transaction(step),
                    ..CommitRequest::default()
                };
                self.client.commit(owner(request)).await.map(|_| None)
            }
            "batchWrite" => {
                let request = BatchWriteRequest {
                    database,
                    writes: writes(&self, step),
                    ..BatchWriteRequest::default()
                };
                self.client.batch_write(owner(request)).await.map(|r| {
                    Some(Json::Array(
                        r.into_inner()
                            .status
                            .iter()
                            .map(|s| {
                                json!({"status": code_name(Code::from_i32(s.code)), "message": s.message})
                            })
                            .collect(),
                    ))
                })
            }
            "rollback" => {
                let request = RollbackRequest {
                    database,
                    transaction: self.transaction(step),
                    ..RollbackRequest::default()
                };
                self.client.rollback(owner(request)).await.map(|_| None)
            }
            "reset" => {
                let mut stream = TcpStream::connect(self.addr).await.unwrap();
                stream
                    .write_all(
                        b"POST /reset HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).await.unwrap();
                assert!(response.starts_with("HTTP/1.1 200"), "{response}");
                Ok(None)
            }
            op => panic!("unknown op {op}"),
        };
        (result, opened)
    }
}

fn writes(context: &Context, step: &Json) -> Vec<Write> {
    step["writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| context.write(w))
        .collect()
}

fn outcome(result: Result<Option<Json>, Status>, elapsed: Duration) -> Json {
    let elapsed = (elapsed.as_secs_f64() * 2.0).round() / 2.0;
    match result {
        Ok(result) => {
            let mut outcome = json!({"status": "OK", "message": "", "elapsed": elapsed});
            if let Some(result) = result {
                outcome["result"] = result;
            }
            outcome
        }
        Err(status) => {
            json!({"status": code_name(status.code()), "message": status.message(), "elapsed": elapsed})
        }
    }
}

/// `value` with every value under `key` replaced by a placeholder.
fn masked(value: &Json, key: &str) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if k == key {
                        json!("<masked>")
                    } else {
                        masked(v, key)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(|v| masked(v, key)).collect()),
        other => other.clone(),
    }
}

/// Transaction IDs differ by design (docs/parity-exceptions.md): compare their presence only.
fn without_ids(value: &Json) -> Json {
    masked(value, "transaction")
}

/// Runs `scenario` on a fresh server; returns one line per step that differs.
async fn replay(index: usize, scenario: Json) -> Vec<String> {
    let (client, addr) = start().await;
    let project = format!("txn-{index}");
    let mut context = Context {
        client,
        addr,
        database: format!("projects/{project}/databases/(default)"),
        transactions: HashMap::new(),
    };
    let name = scenario["name"].as_str().unwrap().to_owned();
    let mut outcomes = HashMap::new();
    let mut background: HashMap<String, JoinHandle<Json>> = HashMap::new();
    for step in scenario["steps"].as_array().unwrap() {
        let label = step["label"].as_str().unwrap().to_owned();
        match step["op"].as_str().unwrap() {
            "sleep" => {
                tokio::time::sleep(Duration::from_secs_f64(step["seconds"].as_f64().unwrap()))
                    .await;
            }
            "wait" => {
                for label in step["for"].as_array().unwrap() {
                    let label = label.as_str().unwrap();
                    let outcome = background.remove(label).unwrap().await.unwrap();
                    outcomes.insert(label.to_owned(), outcome);
                }
            }
            _ if step["background"] == true => {
                let (context, step) = (context.clone(), step.clone());
                background.insert(
                    label,
                    tokio::spawn(async move {
                        let start = Instant::now();
                        let (result, _) = context.execute(&step).await;
                        outcome(result, start.elapsed())
                    }),
                );
            }
            _ => {
                let start = Instant::now();
                let (result, opened) = context.clone().execute(step).await;
                outcomes.insert(label, outcome(result, start.elapsed()));
                context.transactions.extend(opened);
            }
        }
    }
    for (label, handle) in background {
        outcomes.insert(label, handle.await.unwrap());
    }

    let mut differences = Vec::new();
    for step in scenario["steps"].as_array().unwrap() {
        let Some(expected) = step.get("outcome") else {
            continue;
        };
        let label = step["label"].as_str().unwrap();
        let actual = &outcomes[label];
        let mut expected = expected.clone();
        let message = expected["message"]
            .as_str()
            .unwrap()
            .replace("{project}", &project);
        expected["message"] = json!(message);
        let same = match step["compare"].as_str() {
            Some("none") => true,
            Some("status") => expected["status"] == actual["status"],
            Some("codes") => {
                masked(&without_ids(&expected), "message")
                    == masked(&without_ids(actual), "message")
            }
            _ => without_ids(&expected) == without_ids(actual),
        };
        if !same {
            differences.push(format!(
                "{name} / {label}:\n    official {expected}\n    hidane   {actual}"
            ));
        }
    }
    differences
}

fn fixture() -> Json {
    serde_json::from_str(include_str!("fixtures/transactions.json")).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn transactions_behave_like_the_official_emulator() {
    let fixture = fixture();
    let scenarios: Vec<_> = fixture["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, s)| s["slow"] != true)
        .map(|(i, s)| tokio::spawn(replay(i, s.clone())))
        .collect();
    assert!(scenarios.len() >= 15);
    let mut differences = Vec::new();
    for scenario in scenarios {
        differences.extend(scenario.await.unwrap());
    }
    assert!(
        differences.is_empty(),
        "{} steps differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The official emulator fails with `UNKNOWN` and no message for a well-formed ID it never
/// issued, and after a reset for the IDs issued before it. hidane keeps counting across resets,
/// so an old ID has expired, and names a never-issued ID as such (docs/parity-exceptions.md).
#[tokio::test]
async fn unknown_and_stale_transactions() {
    let (client, addr) = start().await;
    let context = Context {
        client: client.clone(),
        addr,
        database: "projects/stale/databases/(default)".to_owned(),
        transactions: HashMap::new(),
    };
    let unknown = json!({"op": "commit", "raw": "EWQAAAAAAAAA", "writes": []});
    let (result, _) = context.clone().execute(&unknown).await;
    let err = result.unwrap_err();
    assert_eq!(
        (err.code(), err.message()),
        (Code::InvalidArgument, "Transaction is invalid or expired.")
    );
    let read = json!({"op": "read", "raw": "EWQAAAAAAAAA", "txn": "x", "documents": ["c/d"]});
    let mut with_raw = context.clone();
    with_raw
        .transactions
        .insert("x".to_owned(), STANDARD.decode("EWQAAAAAAAAA").unwrap());
    let (result, _) = with_raw.execute(&read).await;
    assert_eq!(result.unwrap_err().code(), Code::InvalidArgument);

    let (result, opened) = context
        .clone()
        .execute(&json!({"op": "begin", "as": "T"}))
        .await;
    result.unwrap();
    let mut context = context;
    context.transactions.extend(opened);
    context
        .clone()
        .execute(&json!({"op": "reset"}))
        .await
        .0
        .unwrap();
    let (result, _) = context
        .clone()
        .execute(&json!({"op": "commit", "txn": "T", "writes": []}))
        .await;
    let err = result.unwrap_err();
    assert_eq!(
        (err.code(), err.message()),
        (
            Code::Aborted,
            "The referenced transaction has expired or is no longer valid."
        )
    );
}

/// The official emulator commits each BatchWrite write on its own, so each successful write has
/// its own update time.
#[tokio::test]
async fn batch_write_commits_each_write_separately() {
    let (mut client, _) = start().await;
    let database = "projects/batch/databases/(default)".to_owned();
    let context = Context {
        client: client.clone(),
        addr: "127.0.0.1:1".parse().unwrap(),
        database: database.clone(),
        transactions: HashMap::new(),
    };
    let writes = ["c/a", "c/b", "c/c"]
        .iter()
        .map(|d| context.write(&json!({"update": d, "n": 1})))
        .collect();
    let response = client
        .batch_write(owner(BatchWriteRequest {
            database,
            writes,
            ..BatchWriteRequest::default()
        }))
        .await
        .unwrap()
        .into_inner();
    let times: Vec<_> = response
        .write_results
        .iter()
        .map(|r| {
            let t = r.update_time.unwrap();
            (t.seconds, t.nanos)
        })
        .collect();
    assert!(times.windows(2).all(|w| w[0] < w[1]), "{times:?}");
}
