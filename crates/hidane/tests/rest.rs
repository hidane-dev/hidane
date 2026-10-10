//! REST against the official emulator's recordings (`fixtures/rest.json`, from
//! `tools/oracle/rest.py`): the same requests, in the same order, to a fresh hidane, comparing
//! status, content type and the body byte for byte after the oracle's normalisation, so the
//! JSON layout (protobuf-java JsonFormat's) is checked as well as the values.

use std::net::SocketAddr;

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// As long as the oracle's `rest-HHMMSS`.
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

#[tokio::test]
async fn rest_answers_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/rest.json")).unwrap();
    let addr = start().await;
    let mut differences = Vec::new();
    let cases = fixture["cases"].as_array().unwrap();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let path = case["path"].as_str().unwrap().replace("{project}", PROJECT);
        let body = match (case.get("raw"), case.get("body")) {
            (Some(raw), _) => Some(raw.as_str().unwrap().to_owned()),
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
    assert!(cases.len() > 50);
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
