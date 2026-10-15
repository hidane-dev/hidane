//! RunAggregationQuery against the official emulator's recordings (`fixtures/aggregations.json`,
//! from `tools/oracle/aggregations.py`). The fixture holds the dataset, the requests in REST
//! JSON and the official responses; this test seeds hidane the same way and compares every
//! aggregate value, including its type (integer or double), or the error.
//!
//! `fixtures/aggregations_enterprise.json` holds the same cases on an enterprise database
//! (#338), where a limit or an offset without an order picks documents in no defined order:
//! there only the counts are compared.

mod common;

use common::{
    protojson::{fields, same_fields},
    structured_query::{aggregation, structured_query},
};
use hidane_proto::google::firestore::v1::{
    CommitRequest, Document, RunAggregationQueryRequest, StructuredAggregationQuery, Write,
    firestore_client::FirestoreClient, run_aggregation_query_request, structured_aggregation_query,
    write::Operation,
};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tonic::{Code, transport::Channel};

type Client = FirestoreClient<Channel>;

/// As long as the oracle's `aggregations-HHMMSS`: some messages give byte offsets into names.
const PROJECT: &str = "aggregations-000000";

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

fn documents() -> String {
    format!("projects/{PROJECT}/databases/(default)/documents")
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
    replay(include_str!("fixtures/aggregations.json"), false).await;
}

#[tokio::test]
async fn enterprise_aggregations_answer_like_the_official_emulator() {
    replay(include_str!("fixtures/aggregations_enterprise.json"), true).await;
}

/// The aliases of the case's aggregations whose value its documents decide: on an enterprise
/// database, a limit or an offset picks documents in no defined order where the query's orders
/// tie (they do not end with `__name__`), which leaves only the counts.
fn compared(case: &Json, enterprise: bool) -> Option<Vec<String>> {
    let query = &case["query"];
    let total = query["orderBy"].as_array().is_some_and(|orders| {
        orders
            .last()
            .is_some_and(|o| o["field"]["fieldPath"] == "__name__")
    });
    let picks = (query.get("limit").is_some() || query.get("offset").is_some()) && !total;
    (enterprise && picks).then(|| {
        case["aggregations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a.get("count").is_some())
            .filter_map(|a| a["alias"].as_str().map(str::to_owned))
            .collect()
    })
}

/// On an enterprise database, cases whose integers overflow in some orders of addition only:
/// three integers that overflow only if the largest two come first, and the sums over every
/// collection, which hold the largest integers and NaN.
const ORDER_DEPENDENT: &[&str] = &[
    "count, sum and avg of overflow_back",
    "sum without a collection",
    "avg without a collection",
    "count and sum of every collection",
    "sum of every descendant",
    "ordered by the name, without a collection",
];

async fn replay(text: &str, enterprise: bool) {
    let text = text.replace("{documents}", &documents());
    let fixture: Json = serde_json::from_str(&text).unwrap();
    assert_eq!(
        fixture["edition"].as_str().unwrap_or("standard"),
        if enterprise { "enterprise" } else { "standard" }
    );
    let mut client = start(enterprise).await;
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
        // A sum whose partial sums overflow in some orders and not in others: the official
        // emulator adds in no defined order, and answers either way.
        if enterprise && ORDER_DEPENDENT.contains(&name) {
            continue;
        }
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
                let only = compared(case, enterprise);
                let matches = official.is_some_and(|official| {
                    official.len() == responses.len()
                        && official.iter().zip(&responses).all(|(o, r)| {
                            let result = r.result.as_ref().map(|r| &r.aggregate_fields);
                            match (o.get("result"), result) {
                                (Some(o), Some(r)) => {
                                    let official = fields(&o["aggregateFields"]);
                                    match &only {
                                        None => same_fields(r, &official),
                                        Some(aliases) => {
                                            r.keys().collect::<std::collections::BTreeSet<_>>()
                                                == official.keys().collect()
                                                && aliases.iter().all(|a| {
                                                    same_fields(
                                                        &r.iter()
                                                            .filter(|(k, _)| *k == a)
                                                            .map(|(k, v)| (k.clone(), v.clone()))
                                                            .collect(),
                                                        &official
                                                            .iter()
                                                            .filter(|(k, _)| *k == a)
                                                            .map(|(k, v)| (k.clone(), v.clone()))
                                                            .collect(),
                                                    )
                                                })
                                        }
                                    }
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
