//! What the official emulator ignores or refuses, against its recordings
//! (`fixtures/unsupported.json`, from `tools/oracle/unsupported.py`): `explain_options` is
//! ignored, ExecutePipeline needs the enterprise edition, PartitionQuery is unimplemented.

use std::net::SocketAddr;

use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, ExecutePipelineRequest, PartitionQueryRequest,
    RunAggregationQueryRequest, RunQueryRequest, Value, Write, firestore_client::FirestoreClient,
    run_query_response, value::ValueType, write::Operation,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tonic::{Code, Request, Status, transport::Channel};

const DATABASE: &str = "projects/unsupported/databases/(default)";

type Client = FirestoreClient<Channel>;

async fn start(enterprise: bool) -> (Client, SocketAddr) {
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
    (FirestoreClient::new(channel), addr)
}

fn request<T: prost::Message + Default>(case: &Json) -> Request<T> {
    let pool = DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).unwrap();
    let name = format!(
        "google.firestore.v1.{}Request",
        case["rpc"].as_str().unwrap()
    );
    let descriptor = pool.get_message_by_name(&name).unwrap();
    let message = DynamicMessage::deserialize(descriptor, case["request"].clone()).unwrap();
    let mut request = Request::new(message.transcode_to::<T>().unwrap());
    if let Some(authorization) = case["authorization"].as_str() {
        request
            .metadata_mut()
            .insert("authorization", authorization.parse().unwrap());
    }
    request
}

fn error(status: &Status) -> Json {
    let mut name = String::new();
    for (i, c) in format!("{:?}", status.code()).chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    json!({"code": name, "message": status.message()})
}

fn summary(documents: Vec<String>, aggregations: Vec<Json>, explain: bool, done: bool) -> Json {
    json!({"documents": documents, "aggregations": aggregations, "explain_metrics": explain, "done": done})
}

async fn grpc(client: &mut Client, case: &Json) -> Json {
    let documents = format!("{DATABASE}/documents/");
    match case["rpc"].as_str().unwrap() {
        "RunQuery" => match client.run_query(request::<RunQueryRequest>(case)).await {
            Err(status) => error(&status),
            Ok(response) => {
                let mut stream = response.into_inner();
                let (mut names, mut explain, mut done) = (Vec::new(), false, false);
                loop {
                    match stream.message().await {
                        Ok(Some(r)) => {
                            names.extend(r.document.map(|d| d.name.replacen(&documents, "", 1)));
                            explain |= r.explain_metrics.is_some();
                            done = r.continuation_selector
                                == Some(run_query_response::ContinuationSelector::Done(true));
                        }
                        Ok(None) => break summary(names, Vec::new(), explain, done),
                        Err(status) => break error(&status),
                    }
                }
            }
        },
        "RunAggregationQuery" => {
            match client
                .run_aggregation_query(request::<RunAggregationQueryRequest>(case))
                .await
            {
                Err(status) => error(&status),
                Ok(response) => {
                    let mut stream = response.into_inner();
                    let (mut results, mut explain) = (Vec::new(), false);
                    loop {
                        match stream.message().await {
                            Ok(Some(r)) => {
                                explain |= r.explain_metrics.is_some();
                                if let Some(result) = r.result {
                                    let fields: serde_json::Map<String, Json> = result
                                        .aggregate_fields
                                        .into_iter()
                                        .map(|(alias, value)| match value.value_type {
                                            Some(ValueType::IntegerValue(n)) => {
                                                (alias, json!({"integerValue": n.to_string()}))
                                            }
                                            other => panic!("unexpected {other:?}"),
                                        })
                                        .collect();
                                    results.push(Json::Object(fields));
                                }
                            }
                            Ok(None) => break summary(Vec::new(), results, explain, false),
                            Err(status) => break error(&status),
                        }
                    }
                }
            }
        }
        "ExecutePipeline" => match client
            .execute_pipeline(request::<ExecutePipelineRequest>(case))
            .await
        {
            Err(status) => error(&status),
            Ok(_) => panic!("ExecutePipeline answered"),
        },
        "PartitionQuery" => match client
            .partition_query(request::<PartitionQueryRequest>(case))
            .await
        {
            Err(status) => error(&status),
            Ok(_) => panic!("PartitionQuery answered"),
        },
        other => panic!("unknown RPC {other}"),
    }
}

async fn post(addr: SocketAddr, verb: &str, body: &Json) -> Json {
    let body = body.to_string();
    let request = format!(
        "POST /v1/{DATABASE}/documents:{verb} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer owner\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status: i64 = head[9..12].parse().unwrap();
    // Server times differ by nature; the oracle replaced them the same way.
    let mut normalised = String::new();
    let mut rest = body;
    while let Some(at) = [
        "\"readTime\": \"",
        "\"createTime\": \"",
        "\"updateTime\": \"",
    ]
    .iter()
    .filter_map(|key| rest.find(key).map(|i| (i, key.len())))
    .min()
    {
        let (start, len) = at;
        normalised.push_str(&rest[..start + len]);
        normalised.push_str("<time>");
        rest = &rest[start + len..];
        rest = &rest[rest.find('"').unwrap()..];
    }
    normalised.push_str(rest);
    json!({"status": status, "body": normalised})
}

async fn seed(client: &mut Client) {
    let doc = |id: &str, n: i64| Write {
        operation: Some(Operation::Update(Document {
            name: format!("{DATABASE}/documents/c/{id}"),
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
    };
    client
        .commit(CommitRequest {
            database: DATABASE.to_owned(),
            writes: vec![doc("a", 1), doc("b", 2)],
            ..CommitRequest::default()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn ignored_and_refused_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/unsupported.json")).unwrap();
    let (mut client, addr) = start(false).await;
    seed(&mut client).await;
    let mut differences = Vec::new();
    for case in fixture["grpc"].as_array().unwrap() {
        let actual = grpc(&mut client, case).await;
        if actual != case["outcome"] {
            differences.push(format!(
                "grpc {}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    }
    for case in fixture["rest"].as_array().unwrap() {
        let actual = post(addr, case["verb"].as_str().unwrap(), &case["body"]).await;
        if actual != case["outcome"] {
            differences.push(format!(
                "rest {}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// With `--database-edition enterprise`, pipelines are allowed but not implemented yet.
#[tokio::test]
async fn enterprise_pipelines_are_not_implemented_yet() {
    let (mut client, _) = start(true).await;
    let status = client
        .execute_pipeline(ExecutePipelineRequest {
            database: DATABASE.to_owned(),
            ..ExecutePipelineRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unimplemented);
    assert!(
        status.message().contains("/issues/80"),
        "{}",
        status.message()
    );
}
