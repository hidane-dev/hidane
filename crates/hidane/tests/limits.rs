//! Document limits against the official emulator's recordings (`fixtures/limits.json`, from
//! `tools/oracle/limits.py`): value, name, ID, depth and document size, with their messages and
//! the order they are checked in. Values are stored as small specs, expanded as the oracle does.

use std::net::SocketAddr;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Map, Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const DATABASE: &str = "projects/limits/databases/(default)";

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

fn text(spec: &Json) -> String {
    match spec {
        Json::String(s) => s.clone(),
        spec => spec[1]
            .as_str()
            .unwrap()
            .repeat(usize::try_from(spec[2].as_u64().unwrap()).unwrap()),
    }
}

fn value(spec: &Json) -> Json {
    let n = || usize::try_from(spec[1].as_u64().unwrap()).unwrap();
    match spec[0].as_str().unwrap() {
        "string" => json!({"stringValue": spec[2].as_str().unwrap().repeat(n())}),
        "bytes" => json!({"bytesValue": STANDARD.encode(vec![b'x'; n()])}),
        "int" => json!({"integerValue": "1"}),
        "map" => {
            let fields: Map<String, Json> = spec[1]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), value(v)))
                .collect();
            json!({"mapValue": {"fields": fields}})
        }
        "array" => {
            json!({"arrayValue": {"values": spec[1].as_array().unwrap().iter().map(value).collect::<Vec<_>>()}})
        }
        "nest" => {
            let mixed = spec[2].as_bool().unwrap();
            let mut v = json!({"integerValue": "1"});
            for i in 0..n() {
                v = if mixed && i % 2 == 0 {
                    json!({"arrayValue": {"values": [v]}})
                } else {
                    json!({"mapValue": {"fields": {"m": v}}})
                };
            }
            v
        }
        other => panic!("unknown spec {other}"),
    }
}

async fn commit(addr: SocketAddr, case: &Json) -> Json {
    let path: Vec<String> = case["path"].as_array().unwrap().iter().map(text).collect();
    let fields: Map<String, Json> = case["fields"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), value(v)))
        .collect();
    let body = json!({"writes": [{"update": {
        "name": format!("{DATABASE}/documents/{}", path.join("/")),
        "fields": fields,
    }}]})
    .to_string();
    let request = format!(
        "POST /v1/{DATABASE}/documents:commit HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer owner\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status: u16 = head[9..12].parse().unwrap();
    let message = if status == 200 {
        String::new()
    } else {
        serde_json::from_str::<Json>(body).unwrap()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    json!({"status": status, "message": message})
}

#[tokio::test]
async fn limits_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/limits.json")).unwrap();
    let addr = start().await;
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let actual = commit(addr, case).await;
        if actual != case["outcome"] {
            differences.push(format!(
                "{}:\n    official {}\n    hidane   {actual}",
                case["name"].as_str().unwrap(),
                case["outcome"]
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
