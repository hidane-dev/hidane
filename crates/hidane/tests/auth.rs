//! The `Authorization` header against the official emulator's recordings (`fixtures/auth.json`,
//! from `tools/oracle/auth.py`): which values make an administrator, a user or an error, and
//! where each RPC, stream and REST endpoint reads the header among its other checks.

use std::{net::SocketAddr, time::Duration};

use hidane_proto::google::firestore::v1::{
    BatchGetDocumentsRequest, BatchWriteRequest, BeginTransactionRequest, CommitRequest,
    CreateDocumentRequest, DeleteDocumentRequest, GetDocumentRequest, ListCollectionIdsRequest,
    ListDocumentsRequest, ListenRequest, PartitionQueryRequest, RollbackRequest,
    RunAggregationQueryRequest, RunQueryRequest, UpdateDocumentRequest, WriteRequest,
    firestore_client::FirestoreClient,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_stream::StreamExt as _;
use tonic::{Code, Request, Status, transport::Channel};

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

/// One HTTP/1.1 request; the status and the body.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    authorization: &Json,
    body: &Json,
) -> Json {
    let body = body.as_str().unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(authorization) = authorization.as_str() {
        request.push_str(&format!("Authorization: {authorization}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status: i64 = head[9..12].parse().unwrap();
    json!({"status": status, "body": body})
}

/// The fixture's request (ProtoJSON) as `T`, with the case's headers.
fn request<T: prost::Message + Default>(case: &Json, stream: bool) -> Request<T> {
    let pool = DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).unwrap();
    let name = format!(
        "google.firestore.v1.{}Request",
        case["rpc"].as_str().unwrap()
    );
    let descriptor = pool.get_message_by_name(&name).unwrap();
    let json = if case["request"].is_null() {
        json!({})
    } else {
        case["request"].clone()
    };
    let message = DynamicMessage::deserialize(descriptor, json).unwrap();
    let mut request = Request::new(message.transcode_to::<T>().unwrap());
    if let Some(authorization) = case["authorization"].as_str() {
        request
            .metadata_mut()
            .insert("authorization", authorization.parse().unwrap());
    }
    if stream {
        // What the official emulator needs on a stream; hidane does without.
        request.metadata_mut().insert(
            "google-cloud-resource-prefix",
            "projects/auth/databases/(default)".parse().unwrap(),
        );
    }
    request
}

fn outcome<T>(result: Result<T, Status>) -> Json {
    match result {
        Ok(_) => json!({"code": "OK"}),
        Err(status) => json!({"code": code_name(status.code()), "message": status.message()}),
    }
}

fn code_name(code: Code) -> String {
    // `InvalidArgument` → `INVALID_ARGUMENT`, as the fixture records grpcurl's names.
    let mut name = String::new();
    for (i, c) in format!("{code:?}").chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    name
}

/// A server stream's answer: its first message or its error.
async fn first<T>(response: Result<tonic::Response<tonic::Streaming<T>>, Status>) -> Json {
    match response {
        Ok(response) => outcome(response.into_inner().message().await),
        Err(status) => outcome::<()>(Err(status)),
    }
}

/// A bidirectional stream's request: the case's message, if any, then nothing, but open.
fn bidi<T: prost::Message + Default + 'static>(
    case: &Json,
) -> Request<impl tokio_stream::Stream<Item = T> + use<T>> {
    let (metadata, extensions, message) = request::<T>(case, true).into_parts();
    let messages: Vec<T> = if case["request"].is_null() {
        Vec::new()
    } else {
        vec![message]
    };
    let stream = tokio_stream::iter(messages).chain(tokio_stream::pending());
    Request::from_parts(metadata, extensions, stream)
}

async fn grpc(client: &mut Client, case: &Json) -> Json {
    match case["rpc"].as_str().unwrap() {
        "GetDocument" => outcome(
            client
                .get_document(request::<GetDocumentRequest>(case, false))
                .await,
        ),
        "ListDocuments" => outcome(
            client
                .list_documents(request::<ListDocumentsRequest>(case, false))
                .await,
        ),
        "CreateDocument" => outcome(
            client
                .create_document(request::<CreateDocumentRequest>(case, false))
                .await,
        ),
        "UpdateDocument" => outcome(
            client
                .update_document(request::<UpdateDocumentRequest>(case, false))
                .await,
        ),
        "DeleteDocument" => outcome(
            client
                .delete_document(request::<DeleteDocumentRequest>(case, false))
                .await,
        ),
        "Commit" => outcome(client.commit(request::<CommitRequest>(case, false)).await),
        "BeginTransaction" => outcome(
            client
                .begin_transaction(request::<BeginTransactionRequest>(case, false))
                .await,
        ),
        "Rollback" => outcome(
            client
                .rollback(request::<RollbackRequest>(case, false))
                .await,
        ),
        "BatchWrite" => outcome(
            client
                .batch_write(request::<BatchWriteRequest>(case, false))
                .await,
        ),
        "ListCollectionIds" => outcome(
            client
                .list_collection_ids(request::<ListCollectionIdsRequest>(case, false))
                .await,
        ),
        "PartitionQuery" => outcome(
            client
                .partition_query(request::<PartitionQueryRequest>(case, false))
                .await,
        ),
        "BatchGetDocuments" => {
            first(
                client
                    .batch_get_documents(request::<BatchGetDocumentsRequest>(case, false))
                    .await,
            )
            .await
        }
        "RunQuery" => {
            first(
                client
                    .run_query(request::<RunQueryRequest>(case, false))
                    .await,
            )
            .await
        }
        "RunAggregationQuery" => {
            first(
                client
                    .run_aggregation_query(request::<RunAggregationQueryRequest>(case, false))
                    .await,
            )
            .await
        }
        "Listen" => {
            let response = client.listen(bidi::<ListenRequest>(case)).await;
            tokio::time::timeout(Duration::from_secs(5), first(response))
                .await
                .unwrap()
        }
        "Write" => {
            let response = client.write(bidi::<WriteRequest>(case)).await;
            tokio::time::timeout(Duration::from_secs(5), first(response))
                .await
                .unwrap()
        }
        other => panic!("unknown RPC {other}"),
    }
}

#[tokio::test]
async fn authorization_is_read_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/auth.json")).unwrap();
    let (mut client, addr) = start().await;
    let mut differences = Vec::new();
    let mut check = |kind: &str, case: &Json, actual: Json| {
        if actual != case["outcome"] {
            differences.push(format!(
                "{kind} {}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    };
    let list = "/v1/projects/auth/databases/(default)/documents:listCollectionIds";
    for case in fixture["classify"].as_array().unwrap() {
        let actual = http(addr, "POST", list, &case["authorization"], &json!("{}")).await;
        check("classify", case, actual);
    }
    for case in fixture["grpc"].as_array().unwrap() {
        let actual = grpc(&mut client, case).await;
        check("grpc", case, actual);
    }
    for case in fixture["rest"].as_array().unwrap() {
        let actual = http(
            addr,
            case["method"].as_str().unwrap(),
            case["path"].as_str().unwrap(),
            &case["authorization"],
            &case["body"],
        )
        .await;
        check("rest", case, actual);
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
