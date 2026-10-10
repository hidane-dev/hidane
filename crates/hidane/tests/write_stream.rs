//! The Write stream against the official emulator's recordings (`fixtures/write_stream.json`,
//! from `tools/oracle/write_stream.mjs`). The fixture keeps each scenario's steps next to the
//! official outcome; this test runs the same steps over gRPC and compares every response
//! (token, write results, commit time), error and end of stream. Scenarios run at once, one
//! server each, because one of them waits for a transaction lock.

use std::time::{Duration, Instant};

use hidane_proto::google::firestore::v1::{
    BatchGetDocumentsRequest, BeginTransactionRequest, Document, GetDocumentRequest, Precondition,
    Value, Write, WriteRequest, WriteResponse, batch_get_documents_request,
    document_transform::{FieldTransform, field_transform},
    firestore_client::FirestoreClient,
    precondition::ConditionType,
    value::ValueType,
    write::Operation,
};
use serde_json::{Value as Json, json};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Request, Status, Streaming, transport::Channel};

type Client = FirestoreClient<Channel>;

async fn start() -> Client {
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
    FirestoreClient::new(channel)
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

fn int(n: i64) -> Value {
    Value {
        value_type: Some(ValueType::IntegerValue(n)),
    }
}

fn write(documents: &str, w: &Json) -> Write {
    let mut fields = std::collections::BTreeMap::new();
    if let Some(n) = w["n"].as_i64() {
        fields.insert("n".to_owned(), int(n));
    }
    if w["reserved"] == true {
        fields.insert(
            "__x__".to_owned(),
            Value {
                value_type: Some(ValueType::NullValue(0)),
            },
        );
    }
    let mut transforms = Vec::new();
    if let Some(path) = w["increment"].as_str() {
        transforms.push(FieldTransform {
            field_path: path.to_owned(),
            transform_type: Some(field_transform::TransformType::Increment(int(1))),
        });
    }
    if let Some(path) = w["serverTime"].as_str() {
        transforms.push(FieldTransform {
            field_path: path.to_owned(),
            transform_type: Some(field_transform::TransformType::SetToServerValue(
                field_transform::ServerValue::RequestTime as i32,
            )),
        });
    }
    Write {
        operation: Some(Operation::Update(Document {
            name: format!("{documents}/{}", w["update"].as_str().unwrap()),
            fields,
            ..Document::default()
        })),
        current_document: w["exists"].as_bool().map(|exists| Precondition {
            condition_type: Some(ConditionType::Exists(exists)),
        }),
        update_transforms: transforms,
        ..Write::default()
    }
}

fn response(r: &WriteResponse) -> Json {
    json!({"response": {
        "streamId": !r.stream_id.is_empty(),
        "streamToken": (!r.stream_token.is_empty()).then(|| String::from_utf8_lossy(&r.stream_token).into_owned()),
        "writeResults": r.write_results.iter().map(|w| json!({
            "updateTime": w.update_time.is_some(),
            "transformResults": w.transform_results.len(),
        })).collect::<Vec<_>>(),
        "commitTime": r.commit_time.is_some(),
    }})
}

fn error(status: &Status, project: &str) -> Json {
    json!({"error": {
        "status": code_name(status.code()),
        "message": status.message().replace(project, "{project}"),
    }})
}

/// The fixture writes whole seconds as integers; compare them as floats.
fn seconds_as_f64(outcome: &Json) -> Json {
    let mut outcome = outcome.clone();
    if let Some(elapsed) = outcome.get("elapsed").and_then(Json::as_f64) {
        outcome["elapsed"] = json!(elapsed);
    }
    outcome
}

/// Runs one scenario; returns one line per step that differs.
async fn replay(index: usize, scenario: Json) -> Vec<String> {
    let mut client = start().await;
    let project = format!("ws-000000-{index}");
    let database = format!("projects/{project}/databases/(default)");
    let documents = format!("{database}/documents");
    let name = scenario["name"].as_str().unwrap().to_owned();
    // The stream opens with the first `send`; locks are taken before that.
    let mut sender: Option<mpsc::Sender<WriteRequest>> = None;
    let mut inbound: Option<Streaming<WriteResponse>> = None;
    let mut finished = false;
    let mut last_token = Vec::new();
    let mut differences = Vec::new();
    for step in scenario["steps"].as_array().unwrap() {
        let mut actual = None;
        match step["op"].as_str().unwrap() {
            "send" => {
                let request = WriteRequest {
                    database: match &step["database"] {
                        Json::Bool(true) => database.clone(),
                        Json::String(other) => other.clone(),
                        _ => String::new(),
                    },
                    stream_id: step["streamId"].as_str().unwrap_or_default().to_owned(),
                    stream_token: match step["token"].as_str() {
                        Some("last") => last_token.clone(),
                        Some(literal) => literal.as_bytes().to_vec(),
                        None => Vec::new(),
                    },
                    writes: step["writes"]
                        .as_array()
                        .map(|ws| ws.iter().map(|w| write(&documents, w)).collect())
                        .unwrap_or_default(),
                    labels: step["labels"]
                        .as_object()
                        .map(|l| {
                            l.iter()
                                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    ..WriteRequest::default()
                };
                if sender.is_none() {
                    let (tx, rx) = mpsc::channel(16);
                    tx.send(request).await.unwrap();
                    sender = Some(tx);
                    inbound = Some(
                        client
                            .write(Request::new(ReceiverStream::new(rx)))
                            .await
                            .unwrap()
                            .into_inner(),
                    );
                } else if let Some(tx) = &sender {
                    // The server may already have ended the stream.
                    let _ = tx.send(request).await;
                }
            }
            // Dropping the sender half-closes the request side.
            "close" => sender = None,
            "recv" => {
                let start = Instant::now();
                let stream = inbound.as_mut().unwrap();
                let outcome = if finished {
                    json!({"end": true})
                } else {
                    match tokio::time::timeout(Duration::from_secs(3), stream.message()).await {
                        Err(_) => json!({"timeout": true}),
                        Ok(Ok(Some(r))) => {
                            if !r.stream_token.is_empty() {
                                last_token = r.stream_token.clone();
                            }
                            response(&r)
                        }
                        Ok(Ok(None)) => {
                            finished = true;
                            json!({"end": true})
                        }
                        Ok(Err(status)) => {
                            finished = true;
                            error(&status, &project)
                        }
                    }
                };
                let mut outcome = outcome;
                if outcome.get("timeout").is_none() {
                    outcome["elapsed"] = json!((start.elapsed().as_secs_f64() * 2.0).round() / 2.0);
                }
                actual = Some(outcome);
            }
            "lock" => {
                let transaction = client
                    .begin_transaction(BeginTransactionRequest {
                        database: database.clone(),
                        ..BeginTransactionRequest::default()
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .transaction;
                let mut reads = client
                    .batch_get_documents(BatchGetDocumentsRequest {
                        database: database.clone(),
                        documents: vec![format!(
                            "{documents}/{}",
                            step["document"].as_str().unwrap()
                        )],
                        consistency_selector: Some(
                            batch_get_documents_request::ConsistencySelector::Transaction(
                                transaction,
                            ),
                        ),
                        ..BatchGetDocumentsRequest::default()
                    })
                    .await
                    .unwrap()
                    .into_inner();
                while reads.message().await.unwrap().is_some() {}
            }
            "get" => {
                let got = client
                    .get_document(GetDocumentRequest {
                        name: format!("{documents}/{}", step["document"].as_str().unwrap()),
                        ..GetDocumentRequest::default()
                    })
                    .await;
                actual = Some(match got {
                    Ok(doc) => {
                        let n = match doc
                            .into_inner()
                            .fields
                            .get("n")
                            .and_then(|v| v.value_type.clone())
                        {
                            Some(ValueType::IntegerValue(n)) => json!(n),
                            _ => Json::Null,
                        };
                        json!({"document": {"n": n}})
                    }
                    Err(status) => error(&status, &project),
                });
            }
            op => panic!("unknown op {op}"),
        }
        if let (Some(actual), Some(expected)) = (actual, step.get("outcome").map(seconds_as_f64)) {
            let same = if step["compare"] == "status" {
                actual["error"]["status"] == expected["error"]["status"]
            } else {
                actual == expected
            };
            if !same {
                differences.push(format!(
                    "{name} / {}:\n    official {expected}\n    hidane   {actual}",
                    step["label"].as_str().unwrap_or_default()
                ));
            }
        }
    }
    differences
}

#[tokio::test(flavor = "multi_thread")]
async fn the_write_stream_behaves_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/write_stream.json")).unwrap();
    let scenarios: Vec<_> = fixture["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
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

/// Where hidane differs on purpose (docs/parity-exceptions.md). The official emulator fails a
/// stream without `google-cloud-resource-prefix` metadata with UNKNOWN (every test here sends
/// none), accepts any database name in the handshake, and acknowledges a write to a document
/// of another database without storing it anywhere.
#[tokio::test]
async fn invalid_names_are_rejected_instead_of_ignored() {
    let mut client = start().await;
    let open = |requests: Vec<WriteRequest>| {
        let (tx, rx) = mpsc::channel(16);
        for request in requests {
            tx.try_send(request).unwrap();
        }
        (tx, ReceiverStream::new(rx))
    };

    let (_keep, requests) = open(vec![WriteRequest {
        database: "projects/x".to_owned(),
        ..WriteRequest::default()
    }]);
    let mut stream = client.write(requests).await.unwrap().into_inner();
    let err = stream.message().await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    let database = "projects/ws/databases/(default)".to_owned();
    let (tx, requests) = open(vec![WriteRequest {
        database: database.clone(),
        ..WriteRequest::default()
    }]);
    let mut stream = client.write(requests).await.unwrap().into_inner();
    let handshake = stream.message().await.unwrap().unwrap();
    tx.send(WriteRequest {
        stream_token: handshake.stream_token,
        writes: vec![write(
            "projects/other/databases/(default)/documents",
            &json!({"update": "c/d", "n": 1}),
        )],
        ..WriteRequest::default()
    })
    .await
    .unwrap();
    let err = stream.message().await.unwrap_err();
    assert_eq!(
        (err.code(), err.message()),
        (
            Code::InvalidArgument,
            "Document \"projects/other/databases/(default)/documents/c/d\" is not in database \
             \"projects/ws/databases/(default)\"."
        )
    );
}
