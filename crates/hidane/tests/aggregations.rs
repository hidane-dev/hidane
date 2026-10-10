//! RunAggregationQuery against the official emulator's recordings (`fixtures/aggregations.json`,
//! from `tools/oracle/aggregations.py`). The fixture holds the dataset, the requests in REST
//! JSON and the official responses; this test seeds hidane the same way and compares every
//! aggregate value, including its type (integer or double), or the error.

mod common;

use common::{
    protojson::{fields, same_fields},
    structured_query::{field, structured_query},
};
use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, RunAggregationQueryRequest, StructuredAggregationQuery, Write,
    firestore_client::FirestoreClient,
    run_aggregation_query_request,
    structured_aggregation_query::{
        self, Aggregation,
        aggregation::{Avg, Count, Operator, Sum},
    },
    write::Operation,
};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tonic::{Code, transport::Channel};

type Client = FirestoreClient<Channel>;

/// As long as the oracle's `aggregations-HHMMSS`: some messages give byte offsets into names.
const PROJECT: &str = "aggregations-000000";

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

fn aggregation(json: &Json) -> Aggregation {
    let operator = if let Some(count) = json.get("count") {
        Some(Operator::Count(Count {
            up_to: count["upTo"].as_str().map(|n| n.parse().unwrap()),
        }))
    } else if let Some(sum) = json.get("sum") {
        Some(Operator::Sum(Sum {
            field: sum.get("field").map(field),
        }))
    } else {
        json.get("avg").map(|avg| {
            Operator::Avg(Avg {
                field: avg.get("field").map(field),
            })
        })
    };
    Aggregation {
        operator,
        alias: json["alias"].as_str().unwrap_or_default().to_owned(),
    }
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

#[tokio::test]
async fn aggregations_answer_like_the_official_emulator() {
    let text = include_str!("fixtures/aggregations.json").replace("{documents}", &documents());
    let fixture: Json = serde_json::from_str(&text).unwrap();
    let mut client = start().await;
    for doc in fixture["dataset"].as_array().unwrap() {
        client
            .commit(CommitRequest {
                database: format!("projects/{PROJECT}/databases/(default)"),
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
        let name = case["name"].as_str().unwrap();
        let parent = match case["parent"].as_str().unwrap() {
            "" => documents(),
            p => format!("{}/{p}", documents()),
        };
        let request = RunAggregationQueryRequest {
            parent,
            query_type: Some(
                run_aggregation_query_request::QueryType::StructuredAggregationQuery(
                    StructuredAggregationQuery {
                        aggregations: case["aggregations"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(aggregation)
                            .collect(),
                        query_type: Some(structured_aggregation_query::QueryType::StructuredQuery(
                            structured_query(&case["query"]),
                        )),
                    },
                ),
            ),
            ..RunAggregationQueryRequest::default()
        };
        let expected = &case["outcome"];
        let outcome = match client.run_aggregation_query(request).await {
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
                let official =
                    json!({"status": expected["status"], "message": expected["message"]});
                if actual != official {
                    differences.push(format!(
                        "{name}:\n    official {official}\n    hidane   {actual}"
                    ));
                }
            }
            Ok(responses) => {
                let official = expected["responses"].as_array();
                let matches = official.is_some_and(|official| {
                    official.len() == responses.len()
                        && official.iter().zip(&responses).all(|(o, r)| {
                            let result = r.result.as_ref().map(|r| &r.aggregate_fields);
                            match (o.get("result"), result) {
                                (Some(o), Some(r)) => {
                                    same_fields(r, &fields(&o["aggregateFields"]))
                                }
                                (None, None) => true,
                                _ => false,
                            }
                        })
                });
                if !matches {
                    let actual: Vec<_> = responses
                        .iter()
                        .map(|r| format!("{:?}", r.result))
                        .collect();
                    differences.push(format!(
                        "{name}:\n    official {}\n    hidane   {actual:?}",
                        expected
                    ));
                }
            }
        }
    }
    assert!(cases.len() > 40, "{}", cases.len());
    assert!(
        differences.is_empty(),
        "{} of {} cases differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}
