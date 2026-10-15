//! Pipelines (`ExecutePipeline` on an enterprise database, #80) against the official
//! emulator's recordings (`fixtures/pipelines.json`, from `tools/oracle/pipelines.py`): the
//! dataset is written, then each case runs over REST, in the recorder's order (some update or
//! delete documents), and its outcome is compared: the error, or the results. Results compare in
//! order after a `sort` or `find_nearest`, and as a set otherwise: the official emulator
//! returns them in an order of its own. `PIPELINE_CASES` runs the cases whose name contains it.

use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, header};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;

/// As long as the recorder's `pipes-HHMMSS`.
const PROJECT: &str = "pipes-000000";

fn database() -> String {
    format!("projects/{PROJECT}/databases/(default)")
}

/// Cases whose arrays come in an order the official emulator does not define.
const UNORDERED_ARRAYS: &[&str] = &["aggregate array_agg_distinct over"];

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

async fn post(addr: SocketAddr, path: &str, body: &Json) -> (u16, String) {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("http://{addr}{path}"))
        .header(header::AUTHORIZATION, "Bearer owner")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let response = client.request(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn fill(value: &Json) -> Json {
    serde_json::from_str(&value.to_string().replace("{database}", &database())).unwrap()
}

/// The times of `current_document()`, masked as the recorder masks them.
fn masked(value: Json) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.into_iter()
                .map(|(k, v)| {
                    if k == "__create_time__" || k == "__update_time__" {
                        (k, json!("<time>"))
                    } else {
                        (k, masked(v))
                    }
                })
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.into_iter().map(masked).collect()),
        other => other,
    }
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

/// `value` printed with every object's keys in order: the same for equal values.
fn canonical(value: &Json) -> String {
    match value {
        Json::Object(map) => {
            let mut entries: Vec<(&String, &Json)> = map.iter().collect();
            entries.sort_by_key(|(k, _)| *k);
            let inner: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("{}:{}", json!(k), canonical(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Json::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// Every array value sorted, for the cases in `UNORDERED_ARRAYS`.
fn sorted_arrays(value: Json) -> Json {
    match value {
        Json::Object(mut map) => {
            if let Some(Json::Array(values)) = map.get_mut("values") {
                values.sort_by_key(canonical);
            }
            Json::Object(
                map.into_iter()
                    .map(|(k, v)| (k, sorted_arrays(v)))
                    .collect(),
            )
        }
        Json::Array(items) => Json::Array(items.into_iter().map(sorted_arrays).collect()),
        other => other,
    }
}

/// The outcome of `stages`, as the recorder writes it.
async fn run(addr: SocketAddr, case: &Json) -> Json {
    let mut structured = json!({"pipeline": {"stages": fill(&case["pipeline"])}});
    if let Some(options) = case.get("options") {
        structured["options"] = fill(options);
    }
    let mut body = json!({"structuredPipeline": structured});
    if let Some(Json::Object(request)) = case.get("request") {
        for (k, v) in request {
            body[k] = fill(v);
        }
    }
    let path = format!("/v1/{}/documents:executePipeline", database());
    let (status, text) = post(addr, &path, &body).await;
    let text = text.replace(PROJECT, "{project}");
    if status != 200 {
        let error = &serde_json::from_str::<Json>(&text).unwrap()["error"];
        return json!({
            "status": error["status"],
            "message": error.get("message").cloned().unwrap_or(json!("")),
        });
    }
    let responses: Vec<Json> = serde_json::from_str(&text).unwrap();
    let mut results = Vec::new();
    let mut timed = false;
    for response in responses {
        timed |= response.get("executionTime").is_some();
        for doc in response["results"].as_array().into_iter().flatten() {
            let mut out = serde_json::Map::new();
            if let Some(name) = doc.get("name").and_then(Json::as_str) {
                let prefix = "projects/{project}/databases/(default)/documents/";
                out.insert("name".into(), json!(name.trim_start_matches(prefix)));
            }
            out.insert(
                "fields".into(),
                masked(doc.get("fields").cloned().unwrap_or(json!({}))),
            );
            for key in ["createTime", "updateTime"] {
                if doc.get(key).is_some() {
                    out.insert(key.into(), json!(true));
                }
            }
            results.push(Json::Object(out));
        }
    }
    json!({"status": "OK", "results": results, "execution_time": timed})
}

/// Whether the results of `case` come in a defined order: after a sort or a nearest-neighbour
/// search that no stage reorders.
fn ordered(case: &Json) -> bool {
    let names: Vec<&str> = case["pipeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    let Some(at) = names
        .iter()
        .rposition(|n| *n == "sort" || *n == "find_nearest")
    else {
        return false;
    };
    !names[at..]
        .iter()
        .any(|n| matches!(*n, "aggregate" | "distinct" | "union" | "sample"))
}

fn comparable(case: &Json, outcome: Json) -> Json {
    let mut outcome = numbers(outcome);
    let name = case["name"].as_str().unwrap();
    if UNORDERED_ARRAYS
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        outcome = sorted_arrays(outcome);
    }
    if !ordered(case)
        && let Some(Json::Array(results)) = outcome.get_mut("results")
    {
        results.sort_by_key(canonical);
    }
    outcome
}

#[tokio::test]
async fn pipelines_run_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/pipelines.json")).unwrap();
    let addr = start().await;
    let writes: Vec<Json> = fixture["dataset"]
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| {
            json!({"update": {
                "name": format!("{}/documents/{}", database(), doc["name"].as_str().unwrap()),
                "fields": fill(&doc["fields"]),
            }})
        })
        .collect();
    let (status, text) = post(
        addr,
        &format!("/v1/{}/documents:commit", database()),
        &json!({"writes": writes}),
    )
    .await;
    assert_eq!(status, 200, "{text}");

    let filter = std::env::var("PIPELINE_CASES").unwrap_or_default();
    let mut differences = Vec::new();
    let mut compared = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mutates = case["pipeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| matches!(s["name"].as_str(), Some("update" | "delete")));
        if !filter.is_empty() && !name.contains(&filter) && !mutates {
            continue;
        }
        let actual = comparable(case, run(addr, case).await);
        if !filter.is_empty() && !name.contains(&filter) {
            continue;
        }
        compared += 1;
        let expected = comparable(case, case["outcome"].clone());
        if actual != expected {
            differences.push(format!(
                "{name}:\n  official {expected}\n  hidane   {actual}"
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
