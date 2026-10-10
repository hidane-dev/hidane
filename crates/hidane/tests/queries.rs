//! RunQuery against the official emulator's recordings (`fixtures/queries.json`, from
//! `tools/oracle/queries.py`). The fixture holds the dataset, the queries in REST JSON and the
//! official outcome of each: the documents of every response with `skippedResults` and
//! `done`, the fields of projected documents, or the error. This test seeds hidane with the same
//! dataset and compares every case over gRPC.

// Each test binary uses part of the shared helpers.
#[allow(dead_code)]
mod common;

use common::protojson::{fields, same_fields, value};
use hidane_proto::google::firestore::v1::{
    CommitRequest, Cursor, Document, RunQueryRequest, StructuredQuery, Write,
    firestore_client::FirestoreClient,
    run_query_request, run_query_response,
    structured_query::{
        CollectionSelector, CompositeFilter, Direction, FieldFilter, FieldReference, Filter,
        FindNearest, Order, Projection, UnaryFilter, composite_filter, field_filter,
        filter::FilterType, unary_filter,
    },
    write::Operation,
};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tonic::{Code, Request, transport::Channel};

type Client = FirestoreClient<Channel>;

/// As long as the oracle's `queries-HHMMSS`: some messages give byte offsets into names.
const PROJECT: &str = "queries-000000";

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

fn documents() -> String {
    format!("projects/{PROJECT}/databases/(default)/documents")
}

/// The fixture with `{documents}` filled in for this test's project.
fn fixture() -> Json {
    let text = include_str!("fixtures/queries.json").replace("{documents}", &documents());
    serde_json::from_str(&text).unwrap()
}

fn field(json: &Json) -> FieldReference {
    FieldReference {
        field_path: json["fieldPath"].as_str().unwrap_or_default().to_owned(),
    }
}

fn filter(json: &Json) -> Filter {
    let filter_type = if let Some(c) = json.get("compositeFilter") {
        FilterType::CompositeFilter(CompositeFilter {
            op: c["op"]
                .as_str()
                .and_then(composite_filter::Operator::from_str_name)
                .map_or(0, |op| op as i32),
            filters: c["filters"]
                .as_array()
                .map(|fs| fs.iter().map(filter).collect())
                .unwrap_or_default(),
        })
    } else if let Some(f) = json.get("fieldFilter") {
        FilterType::FieldFilter(FieldFilter {
            field: Some(field(&f["field"])),
            op: f["op"]
                .as_str()
                .and_then(field_filter::Operator::from_str_name)
                .map_or(0, |op| op as i32),
            value: f.get("value").map(value),
        })
    } else {
        let u = &json["unaryFilter"];
        FilterType::UnaryFilter(UnaryFilter {
            op: u["op"]
                .as_str()
                .and_then(unary_filter::Operator::from_str_name)
                .map_or(0, |op| op as i32),
            operand_type: Some(unary_filter::OperandType::Field(field(&u["field"]))),
        })
    };
    Filter {
        filter_type: Some(filter_type),
    }
}

fn cursor(json: &Json) -> Cursor {
    Cursor {
        values: json["values"]
            .as_array()
            .map(|vs| vs.iter().map(value).collect())
            .unwrap_or_default(),
        before: json["before"].as_bool().unwrap_or(false),
    }
}

/// A REST `structuredQuery` as the proto message.
fn structured_query(json: &Json) -> StructuredQuery {
    StructuredQuery {
        select: json.get("select").map(|s| Projection {
            fields: s["fields"]
                .as_array()
                .map(|fs| fs.iter().map(field).collect())
                .unwrap_or_default(),
        }),
        from: json["from"]
            .as_array()
            .map(|cs| {
                cs.iter()
                    .map(|c| CollectionSelector {
                        collection_id: c["collectionId"].as_str().unwrap().to_owned(),
                        all_descendants: c["allDescendants"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        r#where: json.get("where").map(filter),
        order_by: json["orderBy"]
            .as_array()
            .map(|os| {
                os.iter()
                    .map(|o| Order {
                        field: Some(field(&o["field"])),
                        direction: o["direction"]
                            .as_str()
                            .and_then(Direction::from_str_name)
                            .map_or(0, |d| d as i32),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        start_at: json.get("startAt").map(cursor),
        end_at: json.get("endAt").map(cursor),
        offset: json["offset"]
            .as_i64()
            .map_or(0, |n| i32::try_from(n).unwrap()),
        limit: json["limit"].as_i64().map(|n| i32::try_from(n).unwrap()),
        find_nearest: json.get("findNearest").map(|_| FindNearest::default()),
    }
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
    let err = client
        .run_query(query(None, Some(ExplainOptions { analyze: false })))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
}
