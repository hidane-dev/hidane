//! Request sizes and the v1beta1 service against the official emulator's recordings
//! (`fixtures/grpc_settings.json`, from `tools/oracle/grpc_settings.py`).

use std::net::SocketAddr;

use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, GetDocumentRequest, ListCollectionIdsRequest, PartitionQueryRequest,
    Value, Write, firestore_client::FirestoreClient, value::ValueType, write::Operation,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tonic::{Request, Status, transport::Channel};

const DATABASE: &str = "projects/grpc-settings/databases/(default)";

async fn start() -> (Channel, SocketAddr) {
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
    (channel, addr)
}

fn pool() -> DescriptorPool {
    DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).unwrap()
}

fn request<T: prost::Message + Default>(case: &Json, name: &str) -> Request<T> {
    let descriptor = pool()
        .get_message_by_name(&format!("google.firestore.v1.{name}"))
        .unwrap();
    let message = DynamicMessage::deserialize(descriptor, case["request"].clone()).unwrap();
    let mut request = Request::new(message.transcode_to::<T>().unwrap());
    if case["owner"].as_bool().unwrap() {
        request
            .metadata_mut()
            .insert("authorization", "Bearer owner".parse().unwrap());
    }
    request
}

/// grpcurl's JSON for a response, with server times masked as the oracle masks them.
fn printed<T: prost::Message>(message: &T, name: &str) -> Json {
    let descriptor = pool()
        .get_message_by_name(&format!("google.firestore.v1.{name}"))
        .unwrap();
    let dynamic = DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice()).unwrap();
    fn mask(value: Json) -> Json {
        match value {
            Json::Object(map) => Json::Object(
                map.into_iter()
                    .map(|(k, v)| {
                        let v = if k.ends_with("Time") {
                            json!("<time>")
                        } else {
                            mask(v)
                        };
                        (k, v)
                    })
                    .collect(),
            ),
            Json::Array(values) => Json::Array(values.into_iter().map(mask).collect()),
            other => other,
        }
    }
    mask(serde_json::to_value(&dynamic).unwrap())
}

fn outcome<T: prost::Message>(result: Result<tonic::Response<T>, Status>, name: &str) -> Json {
    match result {
        Ok(response) => json!({"code": "OK", "response": printed(response.get_ref(), name)}),
        Err(status) => {
            let mut code = String::new();
            for (i, c) in format!("{:?}", status.code()).chars().enumerate() {
                if c.is_uppercase() && i > 0 {
                    code.push('_');
                }
                code.push(c.to_ascii_uppercase());
            }
            json!({"code": code, "message": status.message()})
        }
    }
}

#[tokio::test]
async fn v1beta1_is_v1_under_another_name() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/grpc_settings.json")).unwrap();
    let (channel, _) = start().await;
    let mut v1 = FirestoreClient::new(channel.clone());
    // The generated client calls v1 paths; this one calls the same methods under v1beta1.
    let mut v1beta1 = FirestoreClient::new(
        tower::ServiceBuilder::new()
            .map_request(|mut request: axum::http::Request<tonic::body::Body>| {
                let path = request.uri().path().replacen(
                    "/google.firestore.v1.",
                    "/google.firestore.v1beta1.",
                    1,
                );
                *request.uri_mut() = format!("http://hidane{path}").parse().unwrap();
                request
            })
            .service(channel),
    );
    let mut differences = Vec::new();
    for case in fixture["v1beta1"].as_array().unwrap() {
        let beta = case["service"] == "google.firestore.v1beta1.Firestore";
        let actual = match case["method"].as_str().unwrap() {
            "Commit" => {
                let r = request::<CommitRequest>(case, "CommitRequest");
                let result = if beta {
                    v1beta1.commit(r).await
                } else {
                    v1.commit(r).await
                };
                outcome(result, "CommitResponse")
            }
            "GetDocument" => {
                let r = request::<GetDocumentRequest>(case, "GetDocumentRequest");
                let result = if beta {
                    v1beta1.get_document(r).await
                } else {
                    v1.get_document(r).await
                };
                outcome(result, "Document")
            }
            "ListCollectionIds" => {
                let r = request::<ListCollectionIdsRequest>(case, "ListCollectionIdsRequest");
                let result = if beta {
                    v1beta1.list_collection_ids(r).await
                } else {
                    v1.list_collection_ids(r).await
                };
                outcome(result, "ListCollectionIdsResponse")
            }
            "PartitionQuery" => {
                let r = request::<PartitionQueryRequest>(case, "PartitionQueryRequest");
                let result = if beta {
                    v1beta1.partition_query(r).await
                } else {
                    v1.partition_query(r).await
                };
                outcome(result, "PartitionQueryResponse")
            }
            other => panic!("unknown method {other}"),
        };
        if actual != case["outcome"] {
            differences.push(format!(
                "{}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// Requests larger than tonic's default 4 MiB pass, as on the official emulator (its limit is
/// 100 MiB; the recordings of 64 and 105 MB are not replayed, they are slow to send twice).
#[tokio::test]
async fn large_grpc_requests_pass() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/grpc_settings.json")).unwrap();
    let (channel, _) = start().await;
    let mut client = FirestoreClient::new(channel).max_encoding_message_size(usize::MAX);
    for case in fixture["grpc_sizes"].as_array().unwrap() {
        let megabytes = case["megabytes"].as_u64().unwrap();
        if megabytes > 20 {
            continue;
        }
        let writes = (0..megabytes)
            .map(|i| Write {
                operation: Some(Operation::Update(Document {
                    name: format!("{DATABASE}/documents/sizes/{i}"),
                    fields: [(
                        "s".to_owned(),
                        Value {
                            value_type: Some(ValueType::StringValue("x".repeat(1_000_000))),
                        },
                    )]
                    .into(),
                    ..Document::default()
                })),
                ..Write::default()
            })
            .collect();
        let result = client
            .commit(CommitRequest {
                database: DATABASE.to_owned(),
                writes,
                ..CommitRequest::default()
            })
            .await;
        assert_eq!(
            result.as_ref().map(|_| "OK").map_err(Status::message),
            Ok(case["outcome"]["code"].as_str().unwrap()),
            "{megabytes} MB"
        );
    }
}

/// A REST body of `total` bytes, in writes of at most 900,000-byte strings, like the oracle's.
fn rest_body(total: usize) -> String {
    let name = |i: usize| format!("{DATABASE}/documents/rest/{i}");
    let write = |i: usize, s: &str| json!({"update": {"name": name(i), "fields": {"s": {"stringValue": s}}}});
    let mut writes: Vec<Json> = Vec::new();
    loop {
        writes.push(write(writes.len(), ""));
        if json!({"writes": writes}).to_string().len() + 900_000 > total {
            break;
        }
        let last = writes.len() - 1;
        writes[last] = write(last, &"x".repeat(900_000));
    }
    let last = writes.len() - 1;
    let room = total - json!({"writes": writes}).to_string().len();
    writes[last] = write(last, &"x".repeat(room));
    let body = json!({"writes": writes}).to_string();
    assert_eq!(body.len(), total);
    body
}

#[tokio::test]
async fn rest_bodies_up_to_16_mib() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/grpc_settings.json")).unwrap();
    let (_, addr) = start().await;
    for case in fixture["rest_sizes"].as_array().unwrap() {
        let total = usize::try_from(case["bytes"].as_u64().unwrap()).unwrap();
        let body = rest_body(total);
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST /v1/{DATABASE}/documents:commit HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer owner\r\nContent-Type: application/json\r\nContent-Length: {total}\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        // The server may answer and close before reading everything.
        let _ = stream.write_all(body.as_bytes()).await;
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw).await;
        let raw = String::from_utf8_lossy(&raw);
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let status: i64 = head[9..12].parse().unwrap();
        let content_type = head.lines().find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-type: ")
                .map(str::to_owned)
        });
        let actual = json!({
            "status": status,
            "content_type": content_type,
            "body": if status == 200 { "" } else { body },
        });
        assert_eq!(actual, case["outcome"], "{total} bytes");
    }
}
