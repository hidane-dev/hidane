//! Field order against the official emulator's recordings (`fixtures/field_order.json`, from
//! `tools/oracle/field_order.py`, #108): the same REST requests, in the same order and with
//! their JSON in the same order, to a fresh hidane. Every document returned must hold its
//! fields, nested ones included, in the official emulator's order, and an update must leave
//! `updateTime` at the commit the official emulator left it at.

use std::net::SocketAddr;

use hidane_proto::{
    google::firestore::v1::{
        CommitRequest, Document, DocumentMask, GetDocumentRequest, MapValue, Value, Write,
        firestore_client::FirestoreClient, value::ValueType, write::Operation,
    },
    ordered::Fields,
};
use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tonic::transport::Channel;

async fn start() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    addr
}

async fn send(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> (u16, Json) {
    let body = body.unwrap_or_default();
    let request = format!(
        "{method} /v1/{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut body = &raw[split + 4..];
    let status = head[9..12].parse().unwrap();
    let mut content = Vec::new();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        loop {
            let line = body.windows(2).position(|w| w == b"\r\n").unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&body[..line]).unwrap().trim(), 16)
                    .unwrap();
            if size == 0 {
                break;
            }
            content.extend_from_slice(&body[line + 2..line + 2 + size]);
            body = &body[line + 2 + size + 2..];
        }
    } else {
        content.extend_from_slice(body);
    }
    let json = if content.is_empty() {
        Json::Null
    } else {
        serde_json::from_slice(&content).unwrap()
    };
    (status, json)
}

/// The names of a `fields` object, in order, maps as `name{…}` and arrays of maps as
/// `name[…|…]`.
fn layout(fields: &Json) -> String {
    let Some(fields) = fields.as_object() else {
        return String::new();
    };
    fields
        .iter()
        .map(|(name, value)| {
            if let Some(map) = value.get("mapValue") {
                format!("{name}{{{}}}", layout(&map["fields"]))
            } else if let Some(values) =
                value.pointer("/arrayValue/values").and_then(Json::as_array)
            {
                let maps: Vec<String> = values
                    .iter()
                    .filter_map(|v| v.get("mapValue"))
                    .map(|m| layout(&m["fields"]))
                    .collect();
                if maps.is_empty() {
                    name.clone()
                } else {
                    format!("{name}[{}]", maps.join("|"))
                }
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The documents (or aggregation results) of a response, as layouts with their `updateTime`.
fn documents(response: &Json) -> Vec<(String, Option<String>)> {
    let one = |doc: &Json| {
        (
            layout(&doc["fields"]),
            doc["updateTime"].as_str().map(str::to_owned),
        )
    };
    match response {
        Json::Object(object) if object.contains_key("name") => vec![one(response)],
        Json::Object(object) => object
            .get("documents")
            .and_then(Json::as_array)
            .map(|docs| docs.iter().map(one).collect())
            .unwrap_or_default(),
        Json::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("found")
                    .or_else(|| item.get("document"))
                    .map(one)
                    .or_else(|| {
                        item.pointer("/result/aggregateFields")
                            .map(|fields| (layout(fields), None))
                    })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Which commit of the case an `updateTime` names.
fn commit_index(commits: &[String], update_time: Option<&String>) -> Option<usize> {
    update_time.and_then(|t| commits.iter().position(|c| c == t))
}

#[tokio::test]
async fn fields_come_back_in_the_official_order() {
    let fixture: Vec<Json> =
        serde_json::from_str(include_str!("fixtures/field_order.json")).unwrap();
    let addr = start().await;
    let mut compared = 0;
    for case in &fixture {
        let name = case["name"].as_str().unwrap();
        let mut official_commits = Vec::new();
        let mut our_commits = Vec::new();
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let method = step["method"].as_str().unwrap();
            let path = step["path"].as_str().unwrap();
            let body = (!step["body"].is_null()).then(|| step["body"].to_string());
            let (status, response) = send(addr, method, path, body.as_deref()).await;
            let context = format!("{name}, step {i}: {method} {path}");
            assert_eq!(status, step["status"], "{context}\n{response:#}");
            if status != 200 {
                assert_eq!(
                    response.pointer("/error/message"),
                    step["response"].pointer("/error/message"),
                    "{context}"
                );
                continue;
            }
            if let Some(time) = response["commitTime"].as_str() {
                our_commits.push(time.to_owned());
                official_commits.push(step["response"]["commitTime"].as_str().unwrap().to_owned());
                continue;
            }
            let ours = documents(&response);
            let official = documents(&step["response"]);
            compared += official.len();
            assert_eq!(
                ours.iter().map(|(l, _)| l).collect::<Vec<_>>(),
                official.iter().map(|(l, _)| l).collect::<Vec<_>>(),
                "{context}"
            );
            for ((_, our_time), (_, official_time)) in ours.iter().zip(&official) {
                assert_eq!(
                    commit_index(&our_commits, our_time.as_ref()),
                    commit_index(&official_commits, official_time.as_ref()),
                    "{context}: updateTime"
                );
            }
        }
    }
    // Every read of the fixture was compared, not skipped.
    assert_eq!(compared, 79);
}

fn int(n: i64) -> Value {
    Value {
        value_type: Some(ValueType::IntegerValue(n)),
    }
}

fn names(fields: &Fields) -> Vec<String> {
    fields
        .iter()
        .map(|(name, value)| match &value.value_type {
            Some(ValueType::MapValue(map)) => format!("{name}{:?}", names(&map.fields)),
            _ => name.clone(),
        })
        .collect()
}

/// gRPC carries the same order: hidane's messages keep it on the wire both ways.
#[tokio::test]
async fn grpc_keeps_the_order_too() {
    let addr = start().await;
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = FirestoreClient::new(channel);
    let database = "projects/order/databases/(default)";
    let name = format!("{database}/documents/c/d");
    let update = |fields: Fields, mask: Option<&[&str]>| Write {
        operation: Some(Operation::Update(Document {
            name: name.clone(),
            fields,
            ..Document::default()
        })),
        update_mask: mask.map(|paths| DocumentMask {
            field_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        }),
        ..Write::default()
    };
    let map = |fields: Fields| Value {
        value_type: Some(ValueType::MapValue(MapValue { fields })),
    };
    for write in [
        update(
            Fields::from([
                ("zeta".to_owned(), int(1)),
                ("alpha".to_owned(), int(2)),
                (
                    "mid".to_owned(),
                    map(Fields::from([
                        ("z".to_owned(), int(3)),
                        ("a".to_owned(), int(4)),
                    ])),
                ),
            ]),
            None,
        ),
        update(
            Fields::from([("beta".to_owned(), int(9)), ("alpha".to_owned(), int(10))]),
            Some(&["beta", "alpha"]),
        ),
    ] {
        client
            .commit(CommitRequest {
                database: database.to_owned(),
                writes: vec![write],
                ..CommitRequest::default()
            })
            .await
            .unwrap();
    }
    let get = |mask: Option<&[&str]>| GetDocumentRequest {
        name: name.clone(),
        mask: mask.map(|paths| DocumentMask {
            field_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        }),
        ..GetDocumentRequest::default()
    };
    let doc = client.get_document(get(None)).await.unwrap().into_inner();
    assert_eq!(
        names(&doc.fields),
        ["zeta", r#"mid["z", "a"]"#, "alpha", "beta"]
    );
    let doc = client
        .get_document(get(Some(&["mid.a", "mid.z", "zeta", "alpha"])))
        .await
        .unwrap()
        .into_inner();
    // Buckets of 16: zeta 0, alpha 7, mid 9; inside mid, a 1, z 10.
    assert_eq!(names(&doc.fields), ["zeta", "alpha", r#"mid["a", "z"]"#]);
}
