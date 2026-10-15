//! `--single_project_mode` and `--single_project_mode_error` against the official emulator's
//! recordings (`fixtures/single_project_mode.json`, from `tools/oracle/single_project_mode.py`,
//! #14): each step's answer, over REST, gRPC and WebChannel, and the lines printed on standard
//! output meanwhile.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use hidane_proto::google::firestore::v1::{
    CommitRequest, GetDocumentRequest, ListenRequest, RunQueryRequest, WriteRequest,
    firestore_client::FirestoreClient,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_stream::StreamExt as _;
use tonic::{Code, Request, Status, transport::Channel};

type Client = FirestoreClient<Channel>;
type Lines = Arc<Mutex<Vec<String>>>;

/// A hidane started as `run` was, and what it prints.
async fn start(run: &Json) -> (Client, SocketAddr, Lines) {
    let flags: Vec<&str> = run["flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(
        flags[..4],
        [
            "--project_id",
            "configured",
            "--single_project_mode",
            "true"
        ]
    );
    let error = flags[4..] == ["--single_project_mode_error", "true"];
    let admin = hidane::Admin::default().with_single_project("configured", error);
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    admin
        .single_project()
        .unwrap()
        .set_log(Arc::new(move |line| {
            sink.lock().unwrap().push(line.to_owned())
        }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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
    (FirestoreClient::new(channel), addr, lines)
}

/// One HTTP/1.1 request; the raw status line's code and the body. `None` if no answer came
/// within `wait`.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &str,
    wait: Duration,
) -> Option<(i64, String)> {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(wait, stream.read_to_end(&mut raw))
        .await
        .ok()?
        .unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_owned()
    };
    Some((head[9..12].parse().unwrap(), body))
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let size = usize::from_str_radix(size, 16).unwrap();
        if size == 0 {
            break;
        }
        out.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
    out
}

/// Server times and transaction IDs, as the recorder masks them.
fn normalize(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(at) = rest.find("\": \"") {
        let key_start = rest[..at].rfind('"').map_or(0, |i| i + 1);
        let key = &rest[key_start..at];
        out.push_str(&rest[..at + 4]);
        rest = &rest[at + 4..];
        let mask = match key {
            "createTime" | "updateTime" | "readTime" | "commitTime" => Some("<time>"),
            "transaction" => Some("<id>"),
            _ => None,
        };
        if let Some(mask) = mask {
            out.push_str(mask);
            rest = &rest[rest.find('"').unwrap()..];
        }
    }
    out.push_str(rest);
    out
}

async fn rest(addr: SocketAddr, step: &Json) -> Json {
    let mut headers: Vec<(String, String)> = step["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect();
    let body = step.get("body").map(Json::to_string).unwrap_or_default();
    if step.get("body").is_some() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
    }
    let (status, body) = http(
        addr,
        step["method"].as_str().unwrap(),
        step["path"].as_str().unwrap(),
        &headers,
        &body,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    json!({"status": status, "body": normalize(&body)})
}

/// The step's request (ProtoJSON) as `T`, with the owner's credential and its prefix.
fn request<T: prost::Message + Default>(step: &Json) -> Request<T> {
    let pool = DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).unwrap();
    let name = format!(
        "google.firestore.v1.{}Request",
        step["rpc"].as_str().unwrap()
    );
    let descriptor = pool.get_message_by_name(&name).unwrap();
    let message = DynamicMessage::deserialize(descriptor, step["request"].clone()).unwrap();
    let mut request = Request::new(message.transcode_to::<T>().unwrap());
    let metadata = request.metadata_mut();
    metadata.insert("authorization", "Bearer owner".parse().unwrap());
    if let Some(prefix) = step["prefix"].as_str() {
        metadata.insert("google-cloud-resource-prefix", prefix.parse().unwrap());
    }
    if let Some(params) = step["params"].as_str() {
        metadata.insert("x-goog-request-params", params.parse().unwrap());
    }
    request
}

fn outcome<T>(result: Result<T, Status>) -> Json {
    match result {
        Ok(_) => json!({"code": "OK"}),
        Err(status) => json!({"code": code_name(status.code()), "message": status.message()}),
    }
}

fn code_name(code: Code) -> String {
    // `InvalidArgument` → `INVALID_ARGUMENT`, as the fixture records grpcurl's names.
    let mut name = String::new();
    for (i, c) in format!("{code:?}").chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    name
}

/// A stream's answer: its first message or its error.
async fn first<T>(response: Result<tonic::Response<tonic::Streaming<T>>, Status>) -> Json {
    let first = async {
        match response {
            Ok(response) => outcome(response.into_inner().message().await),
            Err(status) => outcome::<()>(Err(status)),
        }
    };
    tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .unwrap()
}

/// A bidirectional stream's request: the step's message, then nothing, but open.
fn bidi<T: prost::Message + Default + 'static>(
    step: &Json,
) -> Request<impl tokio_stream::Stream<Item = T> + use<T>> {
    let (metadata, extensions, message) = request::<T>(step).into_parts();
    let stream = tokio_stream::iter([message]).chain(tokio_stream::pending());
    Request::from_parts(metadata, extensions, stream)
}

async fn grpc(client: &mut Client, step: &Json) -> Json {
    match step["rpc"].as_str().unwrap() {
        "GetDocument" => outcome(
            client
                .get_document(request::<GetDocumentRequest>(step))
                .await,
        ),
        "Commit" => outcome(client.commit(request::<CommitRequest>(step)).await),
        "RunQuery" => first(client.run_query(request::<RunQueryRequest>(step)).await).await,
        "Write" => first(client.write(bidi::<WriteRequest>(step)).await).await,
        "Listen" => first(client.listen(bidi::<ListenRequest>(step)).await).await,
        other => panic!("unknown RPC {other}"),
    }
}

/// A WebChannel handshake: its first frame, the session ID masked, or that none came.
async fn webchannel(addr: SocketAddr, step: &Json) -> Json {
    let query = format!(
        "VER=8&database={}&RID=1000&CVER=22&X-HTTP-Session-Id=gsessionid&zx=x&t=1",
        encode(step["database"].as_str().unwrap())
    );
    let body = format!(
        "headers={}&count=1&ofs=0&req0___data__={}",
        encode("Authorization:Bearer owner\r\n"),
        encode(&step["message"].to_string())
    );
    let headers = [(
        "Content-Type".to_owned(),
        "application/x-www-form-urlencoded".to_owned(),
    )];
    let path = format!(
        "/google.firestore.v1.Firestore/{}/channel?{query}",
        step["rpc"].as_str().unwrap()
    );
    // The official emulator never answered within five seconds; hidane would at once.
    match http(addr, "POST", &path, &headers, &body, Duration::from_secs(1)).await {
        None => json!({"answered": false}),
        Some((status, body)) => {
            let mut frame: Json = serde_json::from_str(body.split_once('\n').unwrap().1).unwrap();
            frame[0][1][1] = json!("<sid>");
            json!({"answered": true, "status": status, "frame": frame})
        }
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'(' | b')' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn requests_for_another_project_like_the_official_emulator() {
    let fixture: Json =
        serde_json::from_str(include_str!("fixtures/single_project_mode.json")).unwrap();
    let mut differences = Vec::new();
    let mut steps = 0;
    for run in fixture["runs"].as_array().unwrap() {
        let (mut client, addr, lines) = start(run).await;
        for step in run["steps"].as_array().unwrap() {
            steps += 1;
            let answer = match step["kind"].as_str().unwrap() {
                "rest" => rest(addr, step).await,
                "grpc" => grpc(&mut client, step).await,
                "webchannel" => webchannel(addr, step).await,
                other => panic!("unknown kind {other}"),
            };
            let printed: Vec<String> = std::mem::take(&mut *lines.lock().unwrap());
            let expected: Vec<String> = step["stdout"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap().to_owned())
                .collect();
            if answer != step["outcome"] || printed != expected {
                differences.push(format!(
                    "{} / {}:\n    official {}\n{expected:#?}\n    hidane   {answer}\n{printed:#?}",
                    run["name"], step["name"], step["outcome"]
                ));
            }
        }
    }
    assert!(steps > 100);
    assert!(
        differences.is_empty(),
        "{} of {steps} steps differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
