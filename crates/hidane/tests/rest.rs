//! REST against the official emulator's recordings (`fixtures/rest.json`, from
//! `tools/oracle/rest.py`, and the `rest` cases of `fixtures/json_escapes.json`, from
//! `tools/oracle/json_escapes.py`): the same requests, in the same order, to a fresh hidane,
//! comparing status, content type and the body byte for byte after the oracle's normalisation,
//! so the JSON layout (protobuf-java JsonFormat's) and the escaping of strings are checked as
//! well as the values.

use std::net::SocketAddr;

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// As long as the oracles' `rest-HHMMSS` and `json-HHMMSS`.
const PROJECT: &str = "rest-000000";

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

struct Answer {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> Answer {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = body.unwrap_or_default();
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines.next().unwrap()[9..12].parse().unwrap();
    let mut content_type = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line.split_once(": ").unwrap();
        match name.to_ascii_lowercase().as_str() {
            "content-type" => content_type = Some(value.to_owned()),
            "transfer-encoding" => chunked = value == "chunked",
            _ => {}
        }
    }
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_owned()
    };
    Answer {
        status,
        content_type,
        body,
    }
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    loop {
        let (size, rest) = body.split_once("\r\n").unwrap();
        let size = usize::from_str_radix(size.trim(), 16).unwrap();
        if size == 0 {
            return out;
        }
        out.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
}

/// Replaces the string value of every `"key": "…"`.
fn mask_values(text: &str, key: &str, mask: &str) -> String {
    let marker = format!("\"{key}\": \"");
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(&marker) {
        out.push_str(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];
        let end = rest.find('"').unwrap();
        out.push_str(mask);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The oracle's normalisation (`tools/oracle/rest.py`).
fn normalize(text: &str) -> String {
    let mut text = text.replace(PROJECT, "{project}");
    for key in ["createTime", "updateTime", "readTime", "commitTime"] {
        text = mask_values(&text, key, "<time>");
    }
    text = mask_values(&text, "transaction", "<id>");
    text = mask_values(&text, "nextPageToken", "<token>");
    // Generated document IDs: 20 alphanumerics after `documents/gen/`, ending the string.
    let marker = "documents/gen/";
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find(marker) {
        out.push_str(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];
        let id: String = rest.chars().take(21).collect();
        if id.len() == 21
            && id[..20].chars().all(|c| c.is_ascii_alphanumeric())
            && id.ends_with('"')
        {
            out.push_str("<auto>");
            rest = &rest[20..];
        }
    }
    out.push_str(rest);
    out
}

/// Sends `cases` in order to a fresh hidane; the cases that differ.
async fn replay(cases: &[Json]) -> Vec<String> {
    let addr = start().await;
    let mut differences = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let path = case["path"].as_str().unwrap().replace("{project}", PROJECT);
        let body = match (case.get("raw"), case.get("body")) {
            (Some(raw), _) => Some(raw.as_str().unwrap().replace("{project}", PROJECT)),
            (None, Some(body)) => Some(body.to_string().replace("{project}", PROJECT)),
            (None, None) => None,
        };
        let mut headers: Vec<(String, String)> = case["headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
            .collect();
        if body.is_some() && !headers.iter().any(|(k, _)| k == "Content-Type") {
            headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        }
        let answer = send(
            addr,
            case["method"].as_str().unwrap(),
            &path,
            &headers,
            body.as_deref(),
        )
        .await;
        let expected = &case["outcome"];
        let same_head = i64::from(answer.status) == expected["status"].as_i64().unwrap()
            && answer.content_type.as_deref() == expected["contentType"].as_str();
        let same_body = case["compare"] == "status"
            || normalize(&answer.body) == expected["body"].as_str().unwrap();
        if !same_head || !same_body {
            differences.push(format!(
                "{name}:\n    official {} {:?}\n{}\n    hidane   {} {:?}\n{}",
                expected["status"],
                expected["contentType"].as_str(),
                expected["body"].as_str().unwrap(),
                answer.status,
                answer.content_type,
                normalize(&answer.body)
            ));
        }
    }
    differences
}

#[tokio::test]
async fn rest_answers_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/rest.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() > 50);
    let differences = replay(cases).await;
    assert!(
        differences.is_empty(),
        "{} of {} requests differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}

/// Doubles are read correctly rounded (`doubles read correctly rounded` in `fixtures/rest.json`)
/// only with serde_json's `float_roundtrip`. The replay above cannot tell: jsonschema, a
/// dev-dependency, turns that feature on in every test build, so this checks that the build
/// that ships asks for it too (#262).
#[test]
fn shipped_builds_read_doubles_correctly_rounded() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
            .expect("the workspace manifest");
    let serde_json = manifest
        .lines()
        .find(|line| line.starts_with("serde_json = "))
        .expect("serde_json among the workspace dependencies");
    assert!(serde_json.contains("\"float_roundtrip\""), "{serde_json}");
}

/// Strings escaped as the official emulator escapes them, in bodies and in errors
/// (`fixtures/json_escapes.json`, from `tools/oracle/json_escapes.py`, #191).
#[tokio::test]
async fn strings_escape_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/json_escapes.json")).unwrap();
    let cases = fixture["rest"].as_array().unwrap();
    let differences = replay(cases).await;
    assert!(
        differences.is_empty(),
        "{} of {} requests differ:\n{}",
        differences.len(),
        cases.len(),
        differences.join("\n")
    );
}

/// Requests that hang the official emulator, answered instead (docs/parity-exceptions.md).
#[tokio::test]
async fn requests_that_hang_the_official_emulator_are_answered() {
    let addr = start().await;
    let owner = [("Authorization".to_owned(), "Bearer owner".to_owned())];
    let base = "/v1/projects/hang/databases/(default)/documents";
    let created = send(
        addr,
        "PATCH",
        &format!("{base}/c/a"),
        &owner,
        Some(r#"{"fields": {"n": {"integerValue": "1"}}}"#),
    )
    .await;
    assert_eq!(created.status, 200);
    let created: Json = serde_json::from_str(&created.body).unwrap();

    // A query parameter that does not parse.
    for query in ["pageSize=abc", "showMissing=maybe"] {
        let answer = send(addr, "GET", &format!("{base}/c?{query}"), &owner, None).await;
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (
                400,
                r#"{"error":{"code":400,"message":"Payload isn't valid for request.","status":"INVALID_ARGUMENT"}}"#
            ),
            "{query}"
        );
    }

    // A read in a transaction, through a query parameter.
    let begun = send(
        addr,
        "POST",
        &format!("{base}:beginTransaction"),
        &owner,
        Some("{}"),
    )
    .await;
    let transaction = serde_json::from_str::<Json>(&begun.body).unwrap()["transaction"]
        .as_str()
        .unwrap()
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D");
    let read = send(
        addr,
        "GET",
        &format!("{base}/c/a?transaction={transaction}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(read.status, 200, "{}", read.body);

    // `readTime`, which the official emulator always refuses over REST.
    let update_time = created["updateTime"].as_str().unwrap();
    let read = send(
        addr,
        "GET",
        &format!("{base}/c/a?readTime={update_time}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(read.status, 200, "{}", read.body);
    let before = send(
        addr,
        "GET",
        &format!("{base}/c/a?readTime=2026-01-01T00:00:00Z"),
        &owner,
        None,
    )
    .await;
    assert_eq!(before.status, 400, "{}", before.body);

    // A `parent` in the body as well as in the path: the path wins.
    let ids = send(
        addr,
        "POST",
        &format!("{base}:listCollectionIds"),
        &owner,
        Some(r#"{"parent": "projects/hang/databases/(default)/documents/c/a"}"#),
    )
    .await;
    assert_eq!(
        (ids.status, ids.body.as_str()),
        (200, "{\n  \"collectionIds\": [\"c\"]\n}\n")
    );
}

const PAYLOAD: &str = r#"{"error":{"code":400,"message":"Payload isn't valid for request.","status":"INVALID_ARGUMENT"}}"#;

fn owner() -> Vec<(String, String)> {
    vec![("Authorization".to_owned(), "Bearer owner".to_owned())]
}

/// docs/parity-exceptions.md D083: a string with an unpaired surrogate, which the official
/// emulator stores, cannot be held in hidane's UTF-8 strings: the body does not parse. A pair is
/// one character.
#[tokio::test]
async fn a_lone_surrogate_is_a_body_that_does_not_parse() {
    let addr = start().await;
    let base = "/v1/projects/surrogate/databases/(default)/documents";
    for body in [
        r#"{"fields": {"v": {"stringValue": "a\ud800"}}}"#,
        r#"{"fields": {"k\udc00": {"nullValue": null}}}"#,
    ] {
        let answer = send(addr, "PATCH", &format!("{base}/c/d"), &owner(), Some(body)).await;
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (400, PAYLOAD),
            "{body}"
        );
    }
    let pair = r#"{"fields": {"v": {"stringValue": "😀"}}}"#;
    let answer = send(addr, "PATCH", &format!("{base}/c/d"), &owner(), Some(pair)).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let doc: Json = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(doc["fields"]["v"]["stringValue"], "\u{1F600}");
}

/// D084: timestamps are RFC 3339, read strictly; the official emulator rolls month 13 into the
/// next year and reads fields of other widths.
#[tokio::test]
async fn timestamps_are_read_as_rfc_3339() {
    let addr = start().await;
    let base = "/v1/projects/times/databases/(default)/documents";
    let write = |time: &str| format!(r#"{{"fields": {{"t": {{"timestampValue": "{time}"}}}}}}"#);
    for time in [
        "2020-13-01T00:00:00Z",
        "2019-02-29T00:00:00Z",
        "2020-01-01T24:00:00Z",
        "2020-01-01T00:00:00+24:00",
        "2020-1-01T00:00:00Z",
        "2020-01-01T00:00:00.Z",
    ] {
        let answer = send(
            addr,
            "PATCH",
            &format!("{base}/c/d"),
            &owner(),
            Some(&write(time)),
        )
        .await;
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (400, PAYLOAD),
            "{time}"
        );
    }
    let query = r#"{"structuredQuery": {"from": [{"collectionId": "c"}]}, "readTime": "2020-13-01T00:00:00Z"}"#;
    let answer = send(
        addr,
        "POST",
        &format!("{base}:runQuery"),
        &owner(),
        Some(query),
    )
    .await;
    assert_eq!((answer.status, answer.body.as_str()), (400, PAYLOAD));
    for (time, stored) in [
        ("2020-02-29T12:00:00+09:00", "2020-02-29T03:00:00Z"),
        // A leap second, which RFC 3339 allows, is the second before.
        ("2016-12-31T23:59:60Z", "2016-12-31T23:59:59Z"),
    ] {
        let answer = send(
            addr,
            "PATCH",
            &format!("{base}/c/d"),
            &owner(),
            Some(&write(time)),
        )
        .await;
        assert_eq!(answer.status, 200, "{time}: {}", answer.body);
        let doc: Json = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(doc["fields"]["t"]["timestampValue"], stored, "{time}");
    }
}

/// D085: a query that begins a read-write transaction and fails a check made once the
/// transaction has begun leaves no transaction behind, so a write to what it queried does not
/// wait for a lock (on the official emulator, it times out after 2 s).
#[tokio::test]
async fn a_failed_query_leaves_no_lock() {
    let addr = start().await;
    let base = "/v1/projects/lock/databases/(default)/documents";
    for (verb, query) in [
        (
            "runQuery",
            r#"{"structuredQuery": {"from": [{"collectionId": "c"}], "limit": -1}, "newTransaction": {"readWrite": {}}}"#,
        ),
        (
            "runQuery",
            r#"{"structuredQuery": {"from": [{"collectionId": "c"}], "orderBy": [{"field": {"fieldPath": "a"}}, {"field": {"fieldPath": "a"}}]}, "newTransaction": {"readWrite": {}}}"#,
        ),
        (
            "runAggregationQuery",
            r#"{"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "c"}], "offset": -1}, "aggregations": [{"count": {}}]}, "newTransaction": {"readWrite": {}}}"#,
        ),
    ] {
        let failed = send(
            addr,
            "POST",
            &format!("{base}:{verb}"),
            &owner(),
            Some(query),
        )
        .await;
        assert_eq!(failed.status, 400, "{query}: {}", failed.body);
        let started = std::time::Instant::now();
        let write = send(
            addr,
            "PATCH",
            &format!("{base}/c/d"),
            &owner(),
            Some(r#"{"fields": {"a": {"integerValue": "1"}}}"#),
        )
        .await;
        assert_eq!(write.status, 200, "{query}: {}", write.body);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{query}: the write waited {:?}",
            started.elapsed()
        );
    }
}

/// D086: a form-encoded POST, which the official emulator answers with a bare 404, is served:
/// its body is read as JSON.
#[tokio::test]
async fn a_form_encoded_post_is_served() {
    let addr = start().await;
    let base = "/v1/projects/form/databases/(default)/documents";
    for content_type in [
        "application/x-www-form-urlencoded",
        "multipart/form-data; boundary=x",
    ] {
        let mut headers = owner();
        headers.push(("Content-Type".to_owned(), content_type.to_owned()));
        let begun = send(
            addr,
            "POST",
            &format!("{base}:beginTransaction"),
            &headers,
            Some("{}"),
        )
        .await;
        assert_eq!(begun.status, 200, "{content_type}: {}", begun.body);
        let created = send(
            addr,
            "POST",
            &format!("{base}/c?documentId=d"),
            &headers,
            Some("{}"),
        )
        .await;
        assert_eq!(created.status, 200, "{content_type}: {}", created.body);
        let deleted = send(addr, "DELETE", &format!("{base}/c/d"), &owner(), None).await;
        assert_eq!(deleted.status, 200);
        let refused = send(
            addr,
            "POST",
            &format!("{base}:commit"),
            &headers,
            Some("a=b"),
        )
        .await;
        assert_eq!((refused.status, refused.body.as_str()), (400, PAYLOAD));
    }
}

/// D087: a request line longer than 4096 bytes, which the official emulator answers with a bare
/// 404, is served; a URI over 65,534 bytes is refused.
#[tokio::test]
async fn a_long_request_line_is_served() {
    let addr = start().await;
    let base = "/v1/projects/long/databases/(default)/documents";
    let missing = send(
        addr,
        "GET",
        &format!("{base}/c/{}", "x".repeat(5000)),
        &owner(),
        None,
    )
    .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.content_type.as_deref(), Some("application/json"));
    let long_id = "%C3%A9".repeat(751);
    let refused = send(
        addr,
        "PATCH",
        &format!("{base}/c/{long_id}"),
        &owner(),
        Some("{}"),
    )
    .await;
    assert_eq!(refused.status, 400);
    assert!(
        refused
            .body
            .contains("The key path element name is longer than 1500 bytes."),
        "{}",
        refused.body
    );
    let too_long = send(
        addr,
        "GET",
        &format!("{base}/c/{}", "x".repeat(70_000)),
        &owner(),
        None,
    )
    .await;
    assert_eq!(too_long.status, 414);
}

/// D088: a collection group query under a document runs, as production's API allows; the
/// official emulator allows only an ascending key scan there.
#[tokio::test]
async fn a_collection_group_query_runs_under_a_document() {
    let addr = start().await;
    let base = "/v1/projects/group/databases/(default)/documents";
    for (path, a) in [
        ("p/x/q/a", 1),
        ("p/x/r/s/q/b", 2),
        ("p/y/q/c", 3),
        ("q/d", 4),
    ] {
        let body = format!(r#"{{"fields": {{"a": {{"integerValue": "{a}"}}}}}}"#);
        let written = send(
            addr,
            "PATCH",
            &format!("{base}/{path}"),
            &owner(),
            Some(&body),
        )
        .await;
        assert_eq!(written.status, 200);
    }
    let query = r#"{"structuredQuery": {"from": [{"collectionId": "q", "allDescendants": true}], "where": {"fieldFilter": {"field": {"fieldPath": "a"}, "op": "GREATER_THAN", "value": {"integerValue": "0"}}}, "orderBy": [{"field": {"fieldPath": "a"}, "direction": "DESCENDING"}]}}"#;
    let answer = send(
        addr,
        "POST",
        &format!("{base}/p/x:runQuery"),
        &owner(),
        Some(query),
    )
    .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results: Json = serde_json::from_str(&answer.body).unwrap();
    let names: Vec<&str> = results
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["document"]["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "projects/group/databases/(default)/documents/p/x/r/s/q/b",
            "projects/group/databases/(default)/documents/p/x/q/a",
        ]
    );
    let count = r#"{"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "q", "allDescendants": true}], "where": {"fieldFilter": {"field": {"fieldPath": "a"}, "op": "EQUAL", "value": {"integerValue": "1"}}}}, "aggregations": [{"alias": "n", "count": {}}]}}"#;
    let answer = send(
        addr,
        "POST",
        &format!("{base}/p/x:runAggregationQuery"),
        &owner(),
        Some(count),
    )
    .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results: Json = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(
        results[0]["result"]["aggregateFields"]["n"]["integerValue"],
        "1"
    );
}
