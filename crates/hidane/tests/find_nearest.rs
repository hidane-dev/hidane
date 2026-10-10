//! `find_nearest` and vector values against the official emulator's recordings
//! (`fixtures/find_nearest.json`, from `tools/oracle/find_nearest.py`), over REST: results,
//! distances, ties, validation and its order, aggregations, and the checks on stored vectors.

use std::net::SocketAddr;

use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const DOCS: &str = "projects/find-nearest/databases/(default)/documents";

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

/// `POST /v1/{DOCS}:{verb}`: the status and the parsed body.
async fn post(addr: SocketAddr, verb: &str, body: &Json) -> (u16, Json) {
    let body = body.to_string();
    let request = format!(
        "POST /v1/{DOCS}:{verb} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer owner\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    (
        head[9..12].parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}

fn error(status: u16, body: &Json) -> Json {
    json!({"status": status, "message": body["error"]["message"].as_str().unwrap_or_default()})
}

/// The oracle's form of a `:runQuery` answer: names below `documents/` and every field but
/// the vector `v`.
fn documents(status: u16, body: &Json) -> Json {
    if status != 200 {
        return error(status, body);
    }
    body.as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r.get("document"))
        .map(|d| {
            let mut fields = d.get("fields").cloned().unwrap_or_else(|| json!({}));
            fields.as_object_mut().unwrap().remove("v");
            json!({
                "name": d["name"].as_str().unwrap().strip_prefix(DOCS).unwrap().trim_start_matches('/'),
                "fields": fields,
            })
        })
        .collect()
}

#[tokio::test]
async fn find_nearest_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/find_nearest.json")).unwrap();
    let addr = start().await;
    let writes: Vec<Json> = fixture["seed"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, fields)| json!({"update": {"name": format!("{DOCS}/{path}"), "fields": fields}}))
        .collect();
    let (status, body) = post(addr, "commit", &json!({"writes": writes})).await;
    assert_eq!(status, 200, "{body}");

    let mut differences = Vec::new();
    let mut check = |kind: &str, case: &Json, actual: Json| {
        if actual != case["outcome"] {
            differences.push(format!(
                "{kind} {}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    };
    for case in fixture["queries"].as_array().unwrap() {
        let (status, body) =
            post(addr, "runQuery", &json!({"structuredQuery": case["query"]})).await;
        check("query", case, documents(status, &body));
    }
    for case in fixture["aggregations"].as_array().unwrap() {
        let (status, body) = post(addr, "runAggregationQuery", &case["request"]).await;
        let actual = if status == 200 {
            body.as_array()
                .unwrap()
                .iter()
                .filter_map(|r| r.get("result"))
                .map(|r| r["aggregateFields"].clone())
                .collect()
        } else {
            error(status, &body)
        };
        check("aggregation", case, actual);
    }
    for case in fixture["writes"].as_array().unwrap() {
        let write = json!({"writes": [{"update": {"name": format!("{DOCS}/writes/w"), "fields": case["fields"]}}]});
        let (status, body) = post(addr, "commit", &write).await;
        let actual = if status == 200 {
            json!({"status": 200, "message": ""})
        } else {
            error(status, &body)
        };
        check("write", case, actual);
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// A vector field path names a nested field, as in production (the official emulator reads
/// `m.v` as one field named `m.v`; docs/parity-exceptions.md).
#[tokio::test]
async fn a_vector_field_path_is_a_path() {
    let addr = start().await;
    let vector = |x: f64, y: f64| json!({"mapValue": {"fields": {"__type__": {"stringValue": "__vector__"}, "value": {"arrayValue": {"values": [{"doubleValue": x}, {"doubleValue": y}]}}}}});
    let writes = json!({"writes": [
        {"update": {"name": format!("{DOCS}/m/nested"), "fields": {"m": {"mapValue": {"fields": {"v": vector(1.0, 0.0)}}}}}},
        {"update": {"name": format!("{DOCS}/m/dotted"), "fields": {"m.v": vector(0.0, 1.0)}}},
    ]});
    assert_eq!(post(addr, "commit", &writes).await.0, 200);
    for (path, expected) in [("m.v", "m/nested"), ("`m.v`", "m/dotted")] {
        let query = json!({"structuredQuery": {
            "from": [{"collectionId": "m"}],
            "findNearest": {"vectorField": {"fieldPath": path}, "queryVector": vector(1.0, 1.0), "distanceMeasure": "EUCLIDEAN", "limit": 5},
        }});
        let (status, body) = post(addr, "runQuery", &query).await;
        let names: Vec<Json> = documents(status, &body)
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].clone())
            .collect();
        assert_eq!(names, [json!(expected)], "{path}");
    }
}
