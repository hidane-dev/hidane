//! Listen query targets against the official emulator's recordings
//! (`fixtures/listen_queries.json` and, on an enterprise database,
//! `fixtures/listen_queries_enterprise.json`, from `tools/oracle/listen_queries.py`, #338): the
//! queries of `fixtures/queries.json` as the targets of Listen streams, over the same dataset.
//! Each target's documents when it becomes current are compared as a set (a listener's client
//! orders them itself), or the cause of its removal.

mod common;

use std::time::Duration;

use common::{protojson::fields, structured_query::structured_query};
use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, ListenRequest, Target, Write, firestore_client::FirestoreClient,
    listen_request, listen_response, target, target_change::TargetChangeType, write::Operation,
};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tonic::{Request, transport::Channel};

type Client = FirestoreClient<Channel>;

const PROJECT: &str = "listen-000000";

fn database() -> String {
    format!("projects/{PROJECT}/databases/(default)")
}

fn documents() -> String {
    format!("{}/documents", database())
}

async fn start(enterprise: bool) -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default().with_enterprise_edition(enterprise);
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

/// What the target holds when current, or the cause of its removal, or the stream's error.
async fn listen(client: &mut Client, case: &Json) -> Json {
    let parent = match case["parent"].as_str().unwrap() {
        "" => documents(),
        p => format!("{}/{p}", documents()),
    };
    let request = ListenRequest {
        database: database(),
        target_change: Some(listen_request::TargetChange::AddTarget(Target {
            target_id: 2,
            target_type: Some(target::TargetType::Query(target::QueryTarget {
                parent,
                query_type: Some(target::query_target::QueryType::StructuredQuery(
                    structured_query(&case["query"]),
                )),
            })),
            ..Target::default()
        })),
        ..ListenRequest::default()
    };
    // The request stream stays open until the outcome is read, then closes with `sender`.
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender.send(request).await.unwrap();
    let requests = tokio_stream::wrappers::ReceiverStream::new(receiver);
    let error = |status: tonic::Status| json!({"error": {"status": format!("{:?}", status.code()), "message": status.message()}});
    let mut stream = match client.listen(Request::new(requests)).await {
        Ok(response) => response.into_inner(),
        Err(status) => return error(status),
    };
    let _open = sender;
    let mut documents = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(1), stream.message()).await;
        let response = match message {
            Err(_) => return json!({"timeout": true}),
            Ok(Err(status)) => return error(status),
            Ok(Ok(None)) => return json!({"ended": true}),
            Ok(Ok(Some(response))) => response,
        };
        match response.response_type {
            Some(listen_response::ResponseType::DocumentChange(change)) => {
                let name = change.document.unwrap().name;
                documents.push(
                    name.strip_prefix(&format!("{}/", self::documents()))
                        .unwrap()
                        .to_owned(),
                );
            }
            Some(listen_response::ResponseType::TargetChange(change)) => {
                if change.target_change_type == TargetChangeType::Current as i32 {
                    documents.sort();
                    return json!({"documents": documents});
                }
                if change.target_change_type == TargetChangeType::Remove as i32 {
                    let message = change.cause.map(|c| c.message).unwrap_or_default();
                    return json!({"removed": message.replace(PROJECT, "{project}")});
                }
            }
            _ => {}
        }
    }
}

/// Targets that end the stream (D006).
const ENDS_THE_STREAM: &[&str] = &["two collections"];

async fn replay(text: &str, enterprise: bool) {
    let fixture: Json = serde_json::from_str(
        &text
            .replace("{documents}", &documents())
            .replace("{database}", &database()),
    )
    .unwrap();
    assert_eq!(
        fixture["edition"].as_str().unwrap_or("standard"),
        if enterprise { "enterprise" } else { "standard" }
    );
    // The recorder takes the dataset and the queries of `queries.json`: record again when it
    // changes.
    let queries: Json = serde_json::from_str(
        &include_str!("fixtures/queries.json")
            .replace("{documents}", &documents())
            .replace("{database}", &database()),
    )
    .unwrap();
    assert_eq!(
        fixture["dataset"], queries["dataset"],
        "record listen_queries again"
    );
    let names = |f: &Json| -> Vec<Json> {
        f["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].clone())
            .collect()
    };
    assert_eq!(
        names(&fixture),
        names(&queries),
        "record listen_queries again"
    );
    let mut client = start(enterprise).await;
    for doc in fixture["dataset"].as_array().unwrap() {
        client
            .commit(CommitRequest {
                database: database(),
                writes: vec![Write {
                    operation: Some(Operation::Update(Document {
                        name: format!("{}/{}", documents(), doc["name"].as_str().unwrap()),
                        fields: fields(&doc["fields"]),
                        ..Document::default()
                    })),
                    ..Write::default()
                }],
                ..CommitRequest::default()
            })
            .await
            .unwrap();
    }
    let mut differences = Vec::new();
    let cases = fixture["cases"].as_array().unwrap();
    for case in cases {
        let mut expected = case["outcome"].clone();
        if let Some(documents) = expected.get_mut("documents").and_then(Json::as_array_mut) {
            documents.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        }
        let actual = listen(&mut client, case).await;
        let name = case["name"].as_str().unwrap();
        // The official emulator ends these streams with UNKNOWN and no message; hidane ends
        // them with INVALID_ARGUMENT and RunQuery's message (D006).
        if ENDS_THE_STREAM.contains(&name) {
            if expected.get("error").is_none() || actual.get("error").is_none() {
                differences.push(format!("{name}: official {expected}, hidane {actual}"));
            }
            continue;
        }
        if actual != expected {
            differences.push(format!(
                "{}:\n    official {expected}\n    hidane   {actual}",
                case["name"].as_str().unwrap()
            ));
        }
    }
    assert!(cases.len() > 600, "{}", cases.len());
    assert!(
        differences.is_empty(),
        "{} of {} targets differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}

#[tokio::test]
async fn listen_targets_answer_like_the_official_emulator() {
    replay(include_str!("fixtures/listen_queries.json"), false).await;
}

#[tokio::test]
async fn enterprise_listen_targets_answer_like_the_official_emulator() {
    replay(
        include_str!("fixtures/listen_queries_enterprise.json"),
        true,
    )
    .await;
}
