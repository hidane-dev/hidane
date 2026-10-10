//! CORS against the official emulator's recordings (`fixtures/cors.json`, from
//! `tools/oracle/cors.py`): status, every response header but `date`, `content-length` and the
//! hop-by-hop `connection`, and the body, for preflights and ordinary requests with and
//! without `Origin`.

use std::{collections::BTreeMap, net::SocketAddr};

use serde_json::Value as Json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

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

#[tokio::test]
async fn cors_answers_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/cors.json")).unwrap();
    let addr = start().await;
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let method = case["method"].as_str().unwrap();
        let body = case["body"].as_str().unwrap_or_default();
        let mut request = format!(
            "{method} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
            case["path"].as_str().unwrap()
        );
        for (name, value) in case["headers"].as_object().unwrap() {
            request.push_str(&format!("{name}: {}\r\n", value.as_str().unwrap()));
        }
        if case["body"].is_string() {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        request.push_str(body);
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let mut lines = head.lines();
        let status: i64 = lines.next().unwrap()[9..12].parse().unwrap();
        let headers: BTreeMap<String, String> = lines
            .filter_map(|line| line.split_once(": "))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_owned()))
            .filter(|(k, _)| !matches!(k.as_str(), "date" | "content-length" | "connection"))
            .collect();
        let actual = serde_json::json!({"status": status, "headers": headers, "body": body});
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
