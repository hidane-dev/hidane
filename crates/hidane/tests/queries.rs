//! RunQuery against the official emulator's recordings (`fixtures/queries.json`, from
//! `tools/oracle/queries.py`). The fixture holds the dataset, the queries in REST JSON and the
//! official outcome of each: the documents of every response with `skippedResults` and
//! `done`, the fields of projected documents, or the error. This test seeds hidane with the same
//! dataset and compares every case over gRPC.
//!
//! `fixtures/queries_enterprise.json` holds the same cases on an enterprise database (#338),
//! whose results come in no defined order where the query's own orders tie: those are compared
//! on the values the query orders by, and on the documents unless a limit or an offset picks
//! some of them.

mod common;

use common::{
    protojson::{fields, same_fields, value},
    structured_query::structured_query,
};
use hidane_core::order::compare;
use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, RunQueryRequest, RunQueryResponse, Value, Write,
    firestore_client::FirestoreClient, run_query_request, run_query_response, value::ValueType,
    write::Operation,
};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tonic::{Code, Request, transport::Channel};

type Client = FirestoreClient<Channel>;

/// As long as the oracle's `queries-HHMMSS`: some messages give byte offsets into names.
const PROJECT: &str = "queries-000000";

async fn start() -> Client {
    start_edition(false).await
}

async fn start_edition(enterprise: bool) -> Client {
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

fn documents() -> String {
    format!("projects/{PROJECT}/databases/(default)/documents")
}

/// The fixture with `{documents}` filled in for this test's project.
fn fixture() -> Json {
    filled(include_str!("fixtures/queries.json"))
}

fn filled(text: &str) -> Json {
    let text = text.replace("{documents}", &documents()).replace(
        "{database}",
        &format!("projects/{PROJECT}/databases/(default)"),
    );
    serde_json::from_str(&text).unwrap()
}

fn code_name(code: Code) -> String {
    let name = format!("{code:?}");
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

async fn seed(client: &mut Client, dataset: &Json) {
    for doc in dataset.as_array().unwrap() {
        let write = Write {
            operation: Some(Operation::Update(Document {
                name: format!("{}/{}", documents(), doc["name"].as_str().unwrap()),
                fields: fields(&doc["fields"]),
                ..Document::default()
            })),
            ..Write::default()
        };
        client
            .commit(CommitRequest {
                database: format!("projects/{PROJECT}/databases/(default)"),
                writes: vec![write],
                ..CommitRequest::default()
            })
            .await
            .unwrap();
    }
}

/// Runs one case; returns a description of each difference from the official outcome.
async fn replay(client: &mut Client, case: &Json) -> Vec<String> {
    let name = case["name"].as_str().unwrap();
    let parent = match case["parent"].as_str().unwrap() {
        "" => documents(),
        p => format!("{}/{p}", documents()),
    };
    let request = RunQueryRequest {
        parent,
        query_type: Some(run_query_request::QueryType::StructuredQuery(
            structured_query(&case["query"]),
        )),
        ..RunQueryRequest::default()
    };
    let expected = &case["outcome"];
    let mut differences = Vec::new();
    let outcome = match client.run_query(Request::new(request)).await {
        Err(status) => Err(status),
        Ok(stream) => {
            let mut stream = stream.into_inner();
            let mut responses = Vec::new();
            loop {
                match stream.message().await {
                    Ok(Some(r)) => responses.push(r),
                    Ok(None) => break Ok(responses),
                    Err(status) => break Err(status),
                }
            }
        }
    };
    match outcome {
        Err(status) => {
            let actual = json!({
                "status": code_name(status.code()),
                "message": status.message().replace(PROJECT, "{project}"),
            });
            let expected = json!({"status": expected["status"], "message": expected["message"]});
            if actual["status"] != expected["status"] || actual["message"] != expected["message"] {
                differences.push(format!(
                    "{name}:\n    official {expected}\n    hidane   {actual}"
                ));
            }
        }
        Ok(responses) => {
            let shape: Vec<Json> = responses
                .iter()
                .map(|r| {
                    let mut entry = serde_json::Map::new();
                    if let Some(doc) = &r.document {
                        let relative = doc.name.strip_prefix(&format!("{}/", documents())).unwrap();
                        entry.insert("document".into(), json!(relative));
                    }
                    if r.skipped_results > 0 {
                        entry.insert("skipped".into(), json!(r.skipped_results));
                    }
                    if r.continuation_selector
                        == Some(run_query_response::ContinuationSelector::Done(true))
                    {
                        entry.insert("done".into(), json!(true));
                    }
                    Json::Object(entry)
                })
                .collect();
            if expected["status"] != "OK" || expected["responses"] != json!(shape) {
                differences.push(format!(
                    "{name}:\n    official {}\n    hidane   {}",
                    if expected["status"] == "OK" {
                        expected["responses"].to_string()
                    } else {
                        expected.to_string()
                    },
                    json!(shape)
                ));
            } else if let Some(official) = expected.get("fields") {
                for r in &responses {
                    let doc = r.document.as_ref().unwrap();
                    let relative = doc.name.strip_prefix(&format!("{}/", documents())).unwrap();
                    if !same_fields(&doc.fields, &fields(&official[relative])) {
                        differences.push(format!(
                            "{name}: fields of {relative} differ: official {}",
                            official[relative]
                        ));
                    }
                }
            }
        }
    }
    differences
}

#[tokio::test]
async fn queries_answer_like_the_official_emulator() {
    let fixture = fixture();
    let mut client = start().await;
    seed(&mut client, &fixture["dataset"]).await;
    let mut differences = Vec::new();
    let mut replayed = 0;
    for case in fixture["cases"].as_array().unwrap() {
        // find_nearest is #26.
        if case["query"].get("findNearest").is_some() {
            continue;
        }
        replayed += 1;
        differences.extend(replay(&mut client, case).await);
    }
    assert!(replayed > 150, "{replayed}");
    assert!(
        differences.is_empty(),
        "{} of {replayed} cases differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The responses of a RunQuery call, or its error.
async fn run(client: &mut Client, case: &Json) -> Result<Vec<RunQueryResponse>, tonic::Status> {
    let parent = match case["parent"].as_str().unwrap() {
        "" => documents(),
        p => format!("{}/{p}", documents()),
    };
    let request = RunQueryRequest {
        parent,
        query_type: Some(run_query_request::QueryType::StructuredQuery(
            structured_query(&case["query"]),
        )),
        ..RunQueryRequest::default()
    };
    let mut stream = client.run_query(Request::new(request)).await?.into_inner();
    let mut responses = Vec::new();
    while let Some(r) = stream.message().await? {
        responses.push(r);
    }
    Ok(responses)
}

/// The value at `path` (`a.b`, or `__name__`) of the dataset's document `name`.
fn field_of(dataset: &Json, name: &str, path: &str) -> Option<Value> {
    if path == "__name__" {
        return Some(Value {
            value_type: Some(ValueType::ReferenceValue(format!("{}/{name}", documents()))),
        });
    }
    let doc = dataset
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == name)?;
    let mut fields = &doc["fields"];
    let mut found = None;
    for segment in path.split('.') {
        let v = fields.get(segment)?;
        fields = &v["mapValue"]["fields"];
        found = Some(v);
    }
    found.map(value)
}

/// The values the query orders by, of each document in turn.
fn order_keys(dataset: &Json, query: &Json, names: &[String]) -> Vec<Vec<Option<Value>>> {
    let paths: Vec<&str> = query["orderBy"]
        .as_array()
        .map(|orders| {
            orders
                .iter()
                .map(|o| o["field"]["fieldPath"].as_str().unwrap_or(""))
                .collect()
        })
        .unwrap_or_default();
    names
        .iter()
        .map(|name| paths.iter().map(|p| field_of(dataset, name, p)).collect())
        .collect()
}

fn same_keys(a: &[Vec<Option<Value>>], b: &[Vec<Option<Value>>]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.iter().zip(b).all(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => compare(a, b) == std::cmp::Ordering::Equal,
                (None, None) => true,
                _ => false,
            })
        })
}

/// One case on an enterprise database; a description of the difference, if any.
async fn replay_enterprise(client: &mut Client, dataset: &Json, case: &Json) -> Option<String> {
    let name = case["name"].as_str().unwrap();
    let expected = &case["outcome"];
    let differ = |hidane: String| {
        Some(format!(
            "{name}:\n    official {expected}\n    hidane   {hidane}"
        ))
    };
    match run(client, case).await {
        Err(status) => {
            let message = status.message().replace(PROJECT, "{project}");
            // Which document's type an `array_contains` error names depends on the order the
            // official emulator reads them in, which is not defined.
            let official = expected["message"].as_str().unwrap_or("");
            let prefix = |m: &str| m.split(" but got `").next().unwrap_or(m).to_owned();
            if expected["status"] != code_name(status.code())
                || prefix(official) != prefix(&message)
            {
                return differ(format!("{} {message}", code_name(status.code())));
            }
            None
        }
        Ok(responses) => {
            let names: Vec<String> = responses
                .iter()
                .filter_map(|r| r.document.as_ref())
                .map(|d| {
                    d.name
                        .strip_prefix(&format!("{}/", documents()))
                        .unwrap()
                        .to_owned()
                })
                .collect();
            let shape: Vec<Json> = responses
                .iter()
                .map(|r| {
                    let mut entry = serde_json::Map::new();
                    if r.document.is_some() {
                        entry.insert("document".into(), json!("…"));
                    }
                    if r.skipped_results > 0 {
                        entry.insert("skipped".into(), json!(r.skipped_results));
                    }
                    if r.continuation_selector
                        == Some(run_query_response::ContinuationSelector::Done(true))
                    {
                        entry.insert("done".into(), json!(true));
                    }
                    Json::Object(entry)
                })
                .collect();
            if expected["status"] != "OK" {
                return differ(format!("{names:?}"));
            }
            let official: Vec<String> = expected["responses"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| r["document"].as_str().map(str::to_owned))
                .collect();
            let official_shape: Vec<Json> = expected["responses"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    if r.get("document").is_some() {
                        r["document"] = json!("…");
                    }
                    r
                })
                .collect();
            let query = &case["query"];
            let picks = query.get("limit").is_some() || query.get("offset").is_some();
            let sorted = |mut v: Vec<String>| {
                v.sort();
                v
            };
            let same_documents = picks || sorted(official.clone()) == sorted(names.clone());
            if official_shape != shape
                || !same_documents
                || !same_keys(
                    &order_keys(dataset, query, &official),
                    &order_keys(dataset, query, &names),
                )
            {
                return differ(format!("{names:?} {}", json!(shape)));
            }
            if let Some(official) = expected.get("fields") {
                for r in &responses {
                    let doc = r.document.as_ref().unwrap();
                    let relative = doc.name.strip_prefix(&format!("{}/", documents())).unwrap();
                    if official.get(relative).is_some()
                        && !same_fields(&doc.fields, &fields(&official[relative]))
                    {
                        return Some(format!(
                            "{name}: fields of {relative} differ: official {}",
                            official[relative]
                        ));
                    }
                }
            }
            None
        }
    }
}

#[tokio::test]
async fn enterprise_queries_answer_like_the_official_emulator() {
    let fixture = filled(include_str!("fixtures/queries_enterprise.json"));
    assert_eq!(fixture["edition"], "enterprise");
    let mut client = start_edition(true).await;
    seed(&mut client, &fixture["dataset"]).await;
    let mut differences = Vec::new();
    let mut replayed = 0;
    for case in fixture["cases"].as_array().unwrap() {
        if case["query"].get("findNearest").is_some() {
            continue;
        }
        replayed += 1;
        differences.extend(replay_enterprise(&mut client, &fixture["dataset"], case).await);
    }
    assert!(replayed > 150, "{replayed}");
    assert!(
        differences.is_empty(),
        "{} of {replayed} cases differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// `read_time` reads the snapshot of that time; explain options are #26.
#[tokio::test]
async fn read_time_and_explain_options() {
    use hidane_proto::google::firestore::v1::ExplainOptions;
    let mut client = start().await;
    let commit = |n: i64| CommitRequest {
        database: format!("projects/{PROJECT}/databases/(default)"),
        writes: vec![Write {
            operation: Some(Operation::Update(Document {
                name: format!("{}/c/d", documents()),
                fields: fields(&json!({"n": {"integerValue": n.to_string()}})),
                ..Document::default()
            })),
            ..Write::default()
        }],
        ..CommitRequest::default()
    };
    let first = client
        .commit(commit(1))
        .await
        .unwrap()
        .into_inner()
        .commit_time;
    client.commit(commit(2)).await.unwrap();
    let query = |consistency_selector, explain_options| RunQueryRequest {
        parent: documents(),
        query_type: Some(run_query_request::QueryType::StructuredQuery(
            structured_query(&json!({"from": [{"collectionId": "c"}]})),
        )),
        consistency_selector,
        explain_options,
        ..RunQueryRequest::default()
    };
    for (at, expected) in [(first, 1), (None, 2)] {
        let mut stream = client
            .run_query(query(
                at.map(run_query_request::ConsistencySelector::ReadTime),
                None,
            ))
            .await
            .unwrap()
            .into_inner();
        let response = stream.message().await.unwrap().unwrap();
        let doc = response.document.unwrap();
        assert!(same_fields(
            &doc.fields,
            &fields(&json!({"n": {"integerValue": expected.to_string()}}))
        ));
        if let (Some(at), Some(read_time)) = (at, response.read_time) {
            assert_eq!(read_time, at);
        }
    }
    // Explain options are ignored, as on the official emulator: the same result, no metrics.
    let mut stream = client
        .run_query(query(None, Some(ExplainOptions { analyze: true })))
        .await
        .unwrap()
        .into_inner();
    let response = stream.message().await.unwrap().unwrap();
    assert!(same_fields(
        &response.document.unwrap().fields,
        &fields(&json!({"n": {"integerValue": "2"}}))
    ));
    assert!(response.explain_metrics.is_none());
}
