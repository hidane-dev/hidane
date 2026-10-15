//! `:indexUsage` against the official emulator's recordings (`fixtures/index_usage.json`, from
//! `tools/oracle/index_usage.py`, #36): the composite indexes each query of
//! `fixtures/queries.json` and of `fixtures/aggregations.json` needed, each run once in a
//! database of its own seeded with that fixture's dataset; sequences of requests in one
//! database (counts, a failed query, a transaction, a read time, an aggregation, a listener, a
//! reset); and how the endpoint answers. On an enterprise database
//! (`fixtures/index_usage_enterprise.json`), the same sequences. The official emulator lists a
//! database's reports in an order of its own, which changes from run to run, so they compare as
//! a set.

mod common;

use std::{net::SocketAddr, time::Duration};

use bytes::Bytes;
use common::structured_query::structured_query;
use hidane_proto::google::firestore::v1::{
    ListenRequest, Target, firestore_client::FirestoreClient, listen_request, listen_response,
    target, target_change::TargetChangeType,
};
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, header};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tonic::transport::Channel;

/// As long as the recorder's `index-HHMMSS`, since error messages give offsets into names.
const PROJECT: &str = "index-000000";

fn database(n: usize) -> String {
    format!("projects/{PROJECT}/databases/d{n}")
}

async fn start(enterprise: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default().with_enterprise_edition(enterprise);
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    addr
}

struct Answer {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn call(addr: SocketAddr, method: Method, path: &str, body: Option<&Json>) -> Answer {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let mut request = Request::builder()
        .method(method)
        .uri(format!("http://{addr}{path}"))
        .header(header::AUTHORIZATION, "Bearer owner");
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Bytes::new, |b| Bytes::from(b.to_string()));
    let response = client
        .request(request.body(Full::new(body)).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        content_type,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

/// `value` with the recorder's placeholders filled in for `database`.
fn fill(value: &Json, database: &str) -> Json {
    let text = value
        .to_string()
        .replace("{documents}", &format!("{database}/documents"))
        .replace("{database}", database);
    serde_json::from_str(&text).unwrap()
}

async fn seed(addr: SocketAddr, database: &str, dataset: &Json) {
    let writes: Vec<Json> = dataset
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| {
            json!({"update": {
                "name": format!("{database}/documents/{}", doc["name"].as_str().unwrap()),
                "fields": fill(&doc["fields"], database),
            }})
        })
        .collect();
    for chunk in writes.chunks(400) {
        let path = format!("/v1/{database}/documents:commit");
        let answer = call(addr, Method::POST, &path, Some(&json!({"writes": chunk}))).await;
        assert_eq!(answer.status, 200, "{}", answer.body);
    }
}

/// What `:indexUsage` answers for `database`, as the recorder keeps it, the reports sorted.
async fn usage(addr: SocketAddr, database: &str) -> Json {
    let path = format!("/emulator/v1/projects/{PROJECT}:indexUsage?database={database}");
    let answer = call(addr, Method::GET, &path, None).await;
    let text = answer.body.replace(PROJECT, "{project}");
    sorted(json!({"status": answer.status, "body": serde_json::from_str::<Json>(&text).unwrap()}))
}

fn sorted(mut usage: Json) -> Json {
    if let Some(reports) = usage["body"]
        .get_mut("reports")
        .and_then(Json::as_array_mut)
    {
        reports.sort_by_key(Json::to_string);
    }
    usage
}

/// Adds `query` as a Listen target over gRPC: whether it becomes current.
async fn listen(addr: SocketAddr, database: &str, query: &Json) -> bool {
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let request = ListenRequest {
        database: database.to_owned(),
        target_change: Some(listen_request::TargetChange::AddTarget(Target {
            target_id: 2,
            target_type: Some(target::TargetType::Query(target::QueryTarget {
                parent: format!("{database}/documents"),
                query_type: Some(target::query_target::QueryType::StructuredQuery(
                    structured_query(query),
                )),
            })),
            ..Target::default()
        })),
        ..ListenRequest::default()
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender.send(request).await.unwrap();
    let requests = tokio_stream::wrappers::ReceiverStream::new(receiver);
    let Ok(response) = FirestoreClient::new(channel).listen(requests).await else {
        return false;
    };
    let mut stream = response.into_inner();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), stream.message()).await;
        let Ok(Ok(Some(response))) = message else {
            return false;
        };
        if let Some(listen_response::ResponseType::TargetChange(change)) = response.response_type
            && change.target_change_type == TargetChangeType::Current as i32
        {
            return true;
        }
    }
}

/// Runs each sequence in a database of its own, numbered from `n` on, as the recorder did.
async fn replay_sequences(
    addr: SocketAddr,
    sequences: &Json,
    n: &mut usize,
    differences: &mut Vec<String>,
) {
    for sequence in sequences.as_array().unwrap() {
        *n += 1;
        let database = database(*n);
        let write = json!({"writes": [{"update": {
            "name": format!("{database}/documents/c/x"),
            "fields": {"a": {"integerValue": "1"}, "b": {"integerValue": "2"}},
        }}]});
        let path = format!("/v1/{database}/documents:commit");
        let committed = call(addr, Method::POST, &path, Some(&write)).await;
        let committed: Json = serde_json::from_str(&committed.body).unwrap();
        let commit_time = committed["commitTime"].as_str().unwrap();
        let mut answers = Vec::new();
        for request in sequence["requests"].as_array().unwrap() {
            let answer = if let Some(rpc) = request["rpc"].as_str() {
                let body = request["body"]
                    .to_string()
                    .replace("{commitTime}", commit_time);
                let body: Json = serde_json::from_str(&body).unwrap();
                let path = format!("/v1/{database}/documents:{rpc}");
                json!(call(addr, Method::POST, &path, Some(&body)).await.status)
            } else if let Some(query) = request.get("listen") {
                json!(listen(addr, &database, query).await)
            } else {
                json!(call(addr, Method::POST, "/reset", None).await.status)
            };
            answers.push(answer);
        }
        let actual = json!({"answers": answers, "usage": usage(addr, &database).await});
        let expected = json!({
            "answers": sequence["answers"],
            "usage": sorted(sequence["usage"].clone()),
        });
        if actual != expected {
            differences.push(format!(
                "sequence {}:\n  official {expected}\n  hidane   {actual}",
                sequence["name"]
            ));
        }
    }
}

#[tokio::test]
async fn index_usage_answers_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/index_usage.json")).unwrap();
    let addr = start(false).await;
    let mut differences = Vec::new();

    // Each query that ran, in the recorder's order: the queries, then the aggregations.
    let sources = [
        ("query", include_str!("fixtures/queries.json")),
        ("aggregation", include_str!("fixtures/aggregations.json")),
    ];
    let mut recorded = fixture["cases"].as_array().unwrap().iter();
    let mut n = 0;
    for (kind, text) in sources {
        let source: Json = serde_json::from_str(text).unwrap();
        for case in source["cases"].as_array().unwrap() {
            if case["outcome"]["status"] != "OK" {
                continue;
            }
            n += 1;
            let expected = recorded.next().unwrap();
            assert_eq!(expected["name"], case["name"]);
            assert_eq!(expected["kind"], kind);
            let database = database(n);
            seed(addr, &database, &source["dataset"]).await;
            let parent = match case["parent"].as_str().unwrap() {
                "" => format!("{database}/documents"),
                p => format!("{database}/documents/{p}"),
            };
            let query = fill(&case["query"], &database);
            let (path, body) = if kind == "query" {
                (
                    format!("/v1/{parent}:runQuery"),
                    json!({"structuredQuery": query}),
                )
            } else {
                (
                    format!("/v1/{parent}:runAggregationQuery"),
                    json!({"structuredAggregationQuery": {
                        "structuredQuery": query,
                        "aggregations": case["aggregations"],
                    }}),
                )
            };
            let answer = call(addr, Method::POST, &path, Some(&body)).await;
            assert_eq!(answer.status, 200, "{}: {}", case["name"], answer.body);
            let actual = usage(addr, &database).await;
            if actual != sorted(expected["usage"].clone()) {
                differences.push(format!(
                    "{kind} {}:\n  official {}\n  hidane   {actual}",
                    case["name"],
                    sorted(expected["usage"].clone())
                ));
            }
        }
    }
    assert!(
        recorded.next().is_none(),
        "cases the sources no longer hold"
    );

    replay_sequences(addr, &fixture["sequences"], &mut n, &mut differences).await;

    // The database the endpoints read holds one report, so its text compares as it is.
    let used = format!("projects/{PROJECT}/databases/used");
    let query = json!({"structuredQuery": {
        "from": [{"collectionId": "c"}],
        "where": {"compositeFilter": {"op": "AND", "filters": [
            {"fieldFilter": {"field": {"fieldPath": "`a b`"}, "op": "EQUAL", "value": {"integerValue": "1"}}},
            {"fieldFilter": {"field": {"fieldPath": "tags"}, "op": "ARRAY_CONTAINS", "value": {"integerValue": "1"}}},
        ]}},
        "orderBy": [{"field": {"fieldPath": "c"}, "direction": "DESCENDING"}],
    }});
    let path = format!("/v1/{used}/documents:runQuery");
    assert_eq!(
        call(addr, Method::POST, &path, Some(&query)).await.status,
        200
    );
    for endpoint in fixture["endpoints"].as_array().unwrap() {
        let method = Method::from_bytes(endpoint["method"].as_str().unwrap().as_bytes()).unwrap();
        let path = endpoint["path"]
            .as_str()
            .unwrap()
            .replace("{project}", PROJECT);
        let answer = call(addr, method, &path, None).await;
        let actual = json!({
            "status": answer.status,
            "content_type": answer.content_type,
            "body": answer.body.replace(PROJECT, "{project}"),
        });
        let expected = json!({
            "status": endpoint["status"],
            "content_type": endpoint["content_type"],
            "body": endpoint["body"],
        });
        if actual != expected {
            differences.push(format!(
                "endpoint {}:\n  official {expected}\n  hidane   {actual}",
                endpoint["name"]
            ));
        }
    }

    assert!(
        differences.is_empty(),
        "{} of {} differ:\n{}",
        differences.len(),
        n + fixture["endpoints"].as_array().unwrap().len(),
        differences.join("\n")
    );
}

#[tokio::test]
async fn enterprise_index_usage_answers_like_the_official_emulator() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/index_usage_enterprise.json")).unwrap();
    assert_eq!(fixture["edition"], "enterprise");
    let addr = start(true).await;
    let mut differences = Vec::new();
    let mut n = 0;
    replay_sequences(addr, &fixture["sequences"], &mut n, &mut differences).await;
    assert!(
        differences.is_empty(),
        "{} of {n} differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
