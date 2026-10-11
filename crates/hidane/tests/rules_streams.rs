//! The Listen and Write streams under Security Rules, against the official emulator's
//! recordings (`tools/oracle/rules_streams.py`, `fixtures/rules_streams.json`): each case's
//! requests go over gRPC to a fresh hidane, without credentials, and the responses are compared
//! one by one, without times and resume tokens. A denied target is added, then removed with a
//! `PERMISSION_DENIED` cause; a denied write ends the Write stream. Denials are compared as
//! `common::rules::same_denial` says.

mod common;

use std::{net::SocketAddr, time::Duration};

use hidane_proto::google::firestore::v1::{
    ListenRequest, ListenResponse, Target, WriteRequest,
    firestore_client::FirestoreClient,
    listen_request, listen_response,
    target::{DocumentsTarget, QueryTarget, TargetType, query_target},
    target_change::TargetChangeType,
};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Status, Streaming, transport::Channel};

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

/// A REST call as the owner; returns the status.
async fn owner(addr: SocketAddr, method: &str, path: &str, body: &Json) -> u16 {
    let body = body.to_string();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer owner\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response[9..12].parse().unwrap()
}

/// Loads the case's rules and documents, as the oracle did before opening the stream.
async fn set_up(addr: SocketAddr, case: &Json) {
    let database = case["database"].as_str().unwrap();
    let project = database.split('/').nth(1).unwrap();
    let rules =
        json!({"rules": {"files": [{"name": "firestore.rules", "content": case["rules"]}]}});
    let path = format!("/emulator/v1/projects/{project}:securityRules");
    assert_eq!(owner(addr, "PUT", &path, &rules).await, 200);
    for document in case["documents"].as_array().unwrap() {
        let path = format!(
            "/v1/{database}/documents/{}",
            document["path"].as_str().unwrap()
        );
        let fields = json!({"fields": {"n": {"integerValue": document["n"].to_string()}}});
        owner(addr, "DELETE", &path, &Json::Null).await;
        assert_eq!(owner(addr, "PATCH", &path, &fields).await, 200);
    }
}

fn listen_request(json: &Json) -> ListenRequest {
    let target = &json["addTarget"];
    let target_type = match target.get("documents") {
        Some(documents) => TargetType::Documents(DocumentsTarget {
            documents: documents["documents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap().to_owned())
                .collect(),
        }),
        None => TargetType::Query(QueryTarget {
            parent: target["query"]["parent"].as_str().unwrap().to_owned(),
            query_type: Some(query_target::QueryType::StructuredQuery(
                common::structured_query::structured_query(&target["query"]["structuredQuery"]),
            )),
        }),
    };
    ListenRequest {
        database: json["database"].as_str().unwrap().to_owned(),
        target_change: Some(listen_request::TargetChange::AddTarget(Target {
            target_id: i32::try_from(target["targetId"].as_i64().unwrap()).unwrap(),
            target_type: Some(target_type),
            ..Target::default()
        })),
        ..ListenRequest::default()
    }
}

fn write_request(json: &Json) -> WriteRequest {
    WriteRequest {
        database: json["database"].as_str().unwrap_or_default().to_owned(),
        writes: common::protojson::commit_request("", json).writes,
        ..WriteRequest::default()
    }
}

/// What a Listen response says, without times and resume tokens.
#[derive(Debug, PartialEq)]
enum Seen {
    Target {
        change: TargetChangeType,
        ids: Vec<i32>,
        cause: Option<i32>,
    },
    Document {
        name: String,
        ids: Vec<i32>,
        removed: Vec<i32>,
    },
    Write {
        results: usize,
    },
}

fn ids(json: &Json) -> Vec<i32> {
    json.as_array()
        .map(|ids| {
            ids.iter()
                .map(|id| i32::try_from(id.as_i64().unwrap()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

/// The official emulator's response (ProtoJSON), and its cause's message.
fn official(json: &Json) -> (Seen, Option<&str>) {
    if let Some(change) = json.get("targetChange") {
        let kind = change["targetChangeType"].as_str().unwrap_or("NO_CHANGE");
        let seen = Seen::Target {
            change: TargetChangeType::from_str_name(kind).unwrap(),
            ids: ids(&change["targetIds"]),
            cause: change["cause"]["code"]
                .as_i64()
                .map(|c| i32::try_from(c).unwrap()),
        };
        (seen, change["cause"]["message"].as_str())
    } else if let Some(change) = json.get("documentChange") {
        let seen = Seen::Document {
            name: change["document"]["name"].as_str().unwrap().to_owned(),
            ids: ids(&change["targetIds"]),
            removed: ids(&change["removedTargetIds"]),
        };
        (seen, None)
    } else if json.get("streamToken").is_some() {
        let results = json["writeResults"].as_array().map_or(0, Vec::len);
        (Seen::Write { results }, None)
    } else {
        panic!("unexpected response {json}")
    }
}

fn listen_seen(response: &ListenResponse) -> (Seen, Option<String>) {
    match response.response_type.as_ref().unwrap() {
        listen_response::ResponseType::TargetChange(change) => (
            Seen::Target {
                change: change.target_change_type(),
                ids: change.target_ids.clone(),
                cause: change.cause.as_ref().map(|c| c.code),
            },
            change.cause.as_ref().map(|c| c.message.clone()),
        ),
        listen_response::ResponseType::DocumentChange(change) => (
            Seen::Document {
                name: change.document.as_ref().unwrap().name.clone(),
                ids: change.target_ids.clone(),
                removed: change.removed_target_ids.clone(),
            },
            None,
        ),
        other => panic!("unexpected response {other:?}"),
    }
}

/// Reads up to `count` responses. When none comes for a moment, makes `during`'s call once
/// (the oracle made it while the stream was quiet); stops at the end of the stream.
async fn read<T>(
    responses: &mut Streaming<T>,
    count: usize,
    addr: SocketAddr,
    mut during: Option<&Json>,
) -> (Vec<T>, Option<Status>) {
    let mut got = Vec::new();
    while got.len() < count {
        match tokio::time::timeout(Duration::from_millis(500), responses.message()).await {
            Ok(Ok(Some(response))) => got.push(response),
            Ok(Ok(None)) => return (got, None),
            Ok(Err(status)) => return (got, Some(status)),
            Err(_) => match during.take() {
                Some(call) => {
                    let method = call["method"].as_str().unwrap();
                    let path = call["path"].as_str().unwrap();
                    assert_eq!(owner(addr, method, path, &call["body"]).await, 200);
                }
                None => break,
            },
        }
    }
    let end = tokio::time::timeout(Duration::from_millis(500), responses.message()).await;
    (got, end.ok().and_then(Result::err))
}

#[tokio::test]
async fn streams_under_rules() {
    let cases: Vec<Json> =
        serde_json::from_str(include_str!("fixtures/rules_streams.json")).unwrap();
    let (mut client, addr) = start().await;
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        set_up(addr, case).await;
        let requests = case["requests"].as_array().unwrap();
        let expected = case["responses"].as_array().unwrap();
        let during = case.get("during").filter(|d| !d.is_null());
        // The request streams stay open until the case is compared.
        let (listen_tx, listen_rx) = mpsc::channel(4);
        let (write_tx, write_rx) = mpsc::channel(4);
        let (seen, status) = match case["rpc"].as_str().unwrap() {
            "Listen" => {
                for request in requests {
                    listen_tx.send(listen_request(request)).await.unwrap();
                }
                let mut responses = client
                    .listen(Request::new(ReceiverStream::new(listen_rx)))
                    .await
                    .unwrap()
                    .into_inner();
                let (got, status) = read(&mut responses, expected.len(), addr, during).await;
                (got.iter().map(listen_seen).collect::<Vec<_>>(), status)
            }
            "Write" => {
                for request in requests {
                    write_tx.send(write_request(request)).await.unwrap();
                }
                let mut responses = client
                    .write(Request::new(ReceiverStream::new(write_rx)))
                    .await
                    .unwrap()
                    .into_inner();
                let (got, status) = read(&mut responses, expected.len(), addr, during).await;
                let seen = got
                    .iter()
                    .map(|r| {
                        (
                            Seen::Write {
                                results: r.write_results.len(),
                            },
                            None,
                        )
                    })
                    .collect();
                (seen, status)
            }
            other => panic!("unknown rpc {other}"),
        };
        let official_seen: Vec<_> = expected.iter().map(official).collect();
        assert_eq!(
            seen.iter().map(|(s, _)| s).collect::<Vec<_>>(),
            official_seen.iter().map(|(s, _)| s).collect::<Vec<_>>(),
            "{name}"
        );
        for ((_, hidane), (_, official)) in seen.iter().zip(&official_seen) {
            if let (Some(hidane), Some(official)) = (hidane, official) {
                assert!(
                    common::rules::same_denial(hidane, official),
                    "{name}: {hidane:?} against {official:?}"
                );
            }
        }
        match (&status, &case["status"]) {
            (None, Json::Null) => {}
            (Some(status), official) if !official.is_null() => {
                assert_eq!(format!("{:?}", status.code()), official["code"], "{name}");
                let message = official["message"].as_str().unwrap();
                assert!(
                    common::rules::same_denial(status.message(), message),
                    "{name}: {:?} against {message:?}",
                    status.message()
                );
            }
            (status, official) => panic!("{name}: ended with {status:?}, officially {official}"),
        }
        drop((listen_tx, write_tx));
    }
}
