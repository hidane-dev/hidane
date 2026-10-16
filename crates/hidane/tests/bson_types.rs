//! The BSON types of an enterprise database (#338) against the official emulator's recordings
//! (`fixtures/bson_types.json`, from `tools/oracle/bson_types.py`): each value written and read
//! back (or refused), one document per value of every type ordered both ways, each comparison
//! operator against values of every type (the documents it finds, as a set), and what
//! `type()`, `is_type()` and the comparisons of pipelines give for each value.

use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, header};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;

/// As long as the recorder's `bson-HHMMSS`.
const PROJECT: &str = "bson-000000";

fn database() -> String {
    format!("projects/{PROJECT}/databases/(default)")
}

async fn start() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default().with_enterprise_edition(true);
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    addr
}

async fn call(addr: SocketAddr, method: Method, path: &str, body: Option<&Json>) -> (u16, Json) {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let request = Request::builder()
        .method(method)
        .uri(format!("http://{addr}{path}"))
        .header(header::AUTHORIZATION, "Bearer owner")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(
            body.map_or_else(Bytes::new, |b| Bytes::from(b.to_string())),
        ))
        .unwrap();
    let response = client.request(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    (status, json)
}

fn fill(value: &Json) -> Json {
    serde_json::from_str(&value.to_string().replace("{database}", &database())).unwrap()
}

fn outcome(status: u16, body: &Json) -> Json {
    if status == 200 {
        return json!({"status": "OK"});
    }
    json!({
        "status": body["error"]["status"],
        "message": body["error"].get("message").cloned().unwrap_or(json!("")),
    })
}

/// Numbers as doubles: `4.0` and `4` print alike in the recordings.
fn numbers(value: Json) -> Json {
    match value {
        Json::Number(n) => json!(n.as_f64().unwrap()),
        Json::Object(map) => Json::Object(map.into_iter().map(|(k, v)| (k, numbers(v))).collect()),
        Json::Array(items) => Json::Array(items.into_iter().map(numbers).collect()),
        other => other,
    }
}

async fn run_query(addr: SocketAddr, structured: Json) -> Json {
    let path = format!("/v1/{}/documents:runQuery", database());
    let (status, body) = call(
        addr,
        Method::POST,
        &path,
        Some(&json!({"structuredQuery": structured})),
    )
    .await;
    if status != 200 {
        return outcome(status, &body);
    }
    let documents: Vec<Json> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["document"]["name"].as_str())
        .map(|name| json!(name.rsplit('/').next().unwrap()))
        .collect();
    json!({"status": "OK", "documents": documents})
}

fn sorted(mut outcome: Json) -> Json {
    if let Some(Json::Array(documents)) = outcome.get_mut("documents") {
        documents.sort_by_key(Json::to_string);
    }
    outcome
}

#[tokio::test]
async fn bson_types_behave_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/bson_types.json")).unwrap();
    assert_eq!(fixture["edition"], "enterprise");
    let addr = start().await;
    let mut differences = Vec::new();
    let mut compared = 0;

    for (n, write) in fixture["writes"].as_array().unwrap().iter().enumerate() {
        compared += 1;
        let path = format!("w/w{n}");
        let body = json!({"writes": [{"update": {
            "name": format!("{}/documents/{path}", database()),
            "fields": {"v": fill(&write["value"])},
        }}]});
        let (status, answer) = call(
            addr,
            Method::POST,
            &format!("/v1/{}/documents:commit", database()),
            Some(&body),
        )
        .await;
        let mut actual = json!({"outcome": outcome(status, &answer)});
        if status == 200 {
            let (_, doc) = call(
                addr,
                Method::GET,
                &format!("/v1/{}/documents/{path}", database()),
                None,
            )
            .await;
            actual["read"] = doc["fields"]["v"].clone();
        }
        let mut expected = json!({"outcome": write["outcome"]});
        if let Some(read) = write.get("read") {
            expected["read"] = read.clone();
        }
        if numbers(actual.clone()) != numbers(expected.clone()) {
            differences.push(format!(
                "write {}:\n  official {expected}\n  hidane   {actual}",
                write["name"]
            ));
        }
    }

    let writes: Vec<Json> = fixture["values"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            json!({"update": {"name": format!("{}/documents/v/{k}", database()), "fields": {"v": fill(v)}}})
        })
        .collect();
    let (status, answer) = call(
        addr,
        Method::POST,
        &format!("/v1/{}/documents:commit", database()),
        Some(&json!({"writes": writes})),
    )
    .await;
    assert_eq!(status, 200, "{answer}");

    for order in fixture["orders"].as_array().unwrap() {
        compared += 1;
        let actual = run_query(
            addr,
            json!({
                "from": [{"collectionId": "v"}],
                "orderBy": [
                    {"field": {"fieldPath": "v"}, "direction": order["direction"]},
                    {"field": {"fieldPath": "__name__"}, "direction": "ASCENDING"},
                ],
            }),
        )
        .await;
        if actual != order["outcome"] {
            differences.push(format!(
                "order {}:\n  official {}\n  hidane   {actual}",
                order["direction"], order["outcome"]
            ));
        }
    }

    for filter in fixture["filters"].as_array().unwrap() {
        compared += 1;
        let operands: Vec<Json> = filter["operand"]
            .as_str()
            .unwrap()
            .split(", ")
            .map(|o| fixture["operands"][o].clone())
            .collect();
        let value = if matches!(filter["op"].as_str(), Some("IN" | "NOT_IN")) {
            json!({"arrayValue": {"values": operands}})
        } else {
            operands[0].clone()
        };
        let actual = run_query(
            addr,
            json!({
                "from": [{"collectionId": "v"}],
                "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": filter["op"], "value": value}},
            }),
        )
        .await;
        if sorted(actual.clone()) != sorted(filter["outcome"].clone()) {
            differences.push(format!(
                "filter {} {}:\n  official {}\n  hidane   {actual}",
                filter["op"], filter["operand"], filter["outcome"]
            ));
        }
    }

    for pipeline in fixture["pipelines"].as_array().unwrap() {
        compared += 1;
        let document = pipeline["document"].as_str().unwrap();
        let call_function =
            |name: &str, args: Json| json!({"functionValue": {"name": name, "args": args}});
        let field = json!({"fieldReferenceValue": "v"});
        let stages = json!([
            {"name": "documents", "args": [{"referenceValue": format!("/v/{document}")}]},
            {"name": "select", "args": [{"mapValue": {"fields": {
                "t": call_function("type", json!([field])),
                "n": call_function("is_type", json!([field, {"stringValue": "number"}])),
                "e": call_function("equal", json!([field, {"integerValue": "1"}])),
                "lt": call_function("less_than", json!([field, {"mapValue": {"fields": {"__int__": {"integerValue": "2"}}}}])),
            }}}]},
        ]);
        let (status, body) = call(
            addr,
            Method::POST,
            &format!("/v1/{}/documents:executePipeline", database()),
            Some(&json!({"structuredPipeline": {"pipeline": {"stages": stages}}})),
        )
        .await;
        let mut actual = outcome(status, &body);
        if status == 200 {
            let fields: Vec<Json> = body
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|r| r["results"].as_array().cloned().unwrap_or_default())
                .map(|r| r.get("fields").cloned().unwrap_or(json!({})))
                .collect();
            actual["fields"] = json!(fields);
        }
        if actual != pipeline["outcome"] {
            differences.push(format!(
                "pipeline on {document}:\n  official {}\n  hidane   {actual}",
                pipeline["outcome"]
            ));
        }
    }

    assert!(
        differences.is_empty(),
        "{} of {compared} differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
