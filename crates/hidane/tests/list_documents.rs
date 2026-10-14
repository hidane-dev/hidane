//! ListDocuments without a collection ID against the official emulator's recordings
//! (`fixtures/list_documents.json`, from `tools/oracle/list_documents.py`): every collection
//! under the parent in name order, page tokens across collections, the checks and their order,
//! and REST's trailing-slash paths.

use std::net::SocketAddr;

use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, ListDocumentsRequest, Value, Write, firestore_client::FirestoreClient,
    value::ValueType, write::Operation,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tonic::{Request, transport::Channel};

const DOCS: &str = "projects/list/databases/(default)/documents";

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

fn owner<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", "Bearer owner".parse().unwrap());
    request
}

/// The oracle's seed batch: `n` is the index, `m` is true.
async fn commit(client: &mut Client, paths: &Json) -> prost_types::Timestamp {
    let writes = paths
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, path)| Write {
            operation: Some(Operation::Update(Document {
                name: format!("{DOCS}/{}", path.as_str().unwrap()),
                fields: [
                    (
                        "n".to_owned(),
                        ValueType::IntegerValue(i64::try_from(i).unwrap()),
                    ),
                    ("m".to_owned(), ValueType::BooleanValue(true)),
                ]
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        Value {
                            value_type: Some(v),
                        },
                    )
                })
                .collect(),
                ..Document::default()
            })),
            ..Write::default()
        })
        .collect();
    let response = client
        .commit(owner(CommitRequest {
            database: "projects/list/databases/(default)".to_owned(),
            writes,
            ..CommitRequest::default()
        }))
        .await
        .unwrap();
    response.into_inner().commit_time.unwrap()
}

fn relative(name: &str) -> String {
    name.strip_prefix(DOCS)
        .unwrap()
        .trim_start_matches('/')
        .to_owned()
}

async fn list(client: &mut Client, case: &Json, first_commit: &prost_types::Timestamp) -> Json {
    let mut fields = case["request"].as_object().unwrap().clone();
    let pages = fields.remove("pages").is_some();
    let at_first_commit = fields.remove("at_first_commit").is_some();
    let parent = case["parent"].as_str().unwrap();
    fields.insert(
        "parent".to_owned(),
        json!(if parent.is_empty() {
            DOCS.to_owned()
        } else {
            format!("{DOCS}/{parent}")
        }),
    );
    let pool = DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).unwrap();
    let descriptor = pool
        .get_message_by_name("google.firestore.v1.ListDocumentsRequest")
        .unwrap();
    let mut request: ListDocumentsRequest =
        DynamicMessage::deserialize(descriptor, Json::Object(fields))
            .unwrap()
            .transcode_to()
            .unwrap();
    if at_first_commit {
        request.consistency_selector = Some(
            hidane_proto::google::firestore::v1::list_documents_request::ConsistencySelector::ReadTime(
                *first_commit,
            ),
        );
    }
    let mut result = Vec::new();
    loop {
        let mut call = Request::new(request.clone());
        if case["owner"].as_bool().unwrap() {
            call.metadata_mut()
                .insert("authorization", "Bearer owner".parse().unwrap());
        }
        let response = match client.list_documents(call).await {
            Ok(response) => response.into_inner(),
            Err(status) => {
                let code = format!("{:?}", status.code());
                let mut name = String::new();
                for (i, c) in code.chars().enumerate() {
                    if c.is_uppercase() && i > 0 {
                        name.push('_');
                    }
                    name.push(c.to_ascii_uppercase());
                }
                let error = json!({"code": name, "message": status.message()});
                // An error after the first page keeps the pages before it, as recorded.
                return if result.is_empty() {
                    error
                } else {
                    json!({"pages": result, "error": error})
                };
            }
        };
        if !pages {
            let documents: Vec<Json> = response
                .documents
                .iter()
                .map(|d| {
                    let mut fields: Vec<&String> = d.fields.keys().collect();
                    fields.sort();
                    json!({"name": relative(&d.name), "fields": fields})
                })
                .collect();
            return json!({"documents": documents});
        }
        result.push(
            response
                .documents
                .iter()
                .map(|d| relative(&d.name))
                .collect::<Vec<_>>(),
        );
        if response.next_page_token.is_empty() {
            return json!({"pages": result});
        }
        request.page_token = response.next_page_token;
    }
}

async fn get(addr: SocketAddr, suffix: &str) -> Json {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /v1/{DOCS}{suffix} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer owner\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status: i64 = head[9..12].parse().unwrap();
    match serde_json::from_str::<Json>(body) {
        Ok(Json::Object(body)) if body.contains_key("error") => {
            json!({"status": status, "error": body["error"]})
        }
        Ok(body) => json!({
            "status": status,
            "documents": body["documents"]
                .as_array()
                .map(|docs| docs.iter().map(|d| relative(d["name"].as_str().unwrap())).collect::<Vec<_>>())
                .unwrap_or_default(),
            "next_page": body.get("nextPageToken").is_some(),
        }),
        Err(_) => json!({"status": status, "body": body}),
    }
}

/// Cases where hidane answers differently on purpose, with the row of
/// `docs/parity-exceptions.md` that says why: a page token next to an order, which the official
/// emulator answers with `UNKNOWN` and no message ("not supported" in its log) and hidane
/// continues in the requested order, as production does.
const INTENTIONAL: &[(&str, &str)] = &[
    ("collection, order by a field, pages of 2", "D089"),
    ("collection, order by the name, pages of 2", "D089"),
];

/// The same request in one page.
fn unpaged(case: &Json) -> Json {
    let mut case = case.clone();
    let request = case["request"].as_object_mut().unwrap();
    request.remove("pages");
    request.remove("page_size");
    case
}

#[tokio::test]
async fn list_documents_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/list_documents.json")).unwrap();
    let (mut client, addr) = start().await;
    let first_commit = commit(&mut client, &fixture["seed"][0]).await;
    commit(&mut client, &fixture["seed"][1]).await;
    commit(&mut client, &fixture["seed"][2]).await;
    let mut differences = Vec::new();
    for case in fixture["grpc"].as_array().unwrap() {
        let actual = list(&mut client, case, &first_commit).await;
        if INTENTIONAL.iter().any(|(name, _)| case["name"] == *name) {
            // The official emulator's first page, then the rest of the listing in its order.
            let official = &case["outcome"];
            assert_eq!(official["error"]["code"], "UNKNOWN", "{}", case["name"]);
            assert_eq!(actual["pages"][0], official["pages"][0], "{}", case["name"]);
            let all = list(&mut client, &unpaged(case), &first_commit).await;
            let names: Vec<&Json> = all["documents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| &d["name"])
                .collect();
            let paged: Vec<&Json> = actual["pages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|page| page.as_array().unwrap())
                .collect();
            assert_eq!(paged, names, "{}", case["name"]);
            continue;
        }
        if actual != case["outcome"] {
            differences.push(format!(
                "grpc {}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    }
    for case in fixture["rest"].as_array().unwrap() {
        let actual = get(addr, case["path"].as_str().unwrap()).await;
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
