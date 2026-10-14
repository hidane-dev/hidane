//! The WebSocket feed of the Emulator UI's request monitor (`--websocket_port`), against the
//! official emulator's recording (`tools/oracle/websocket_requests.py`,
//! `fixtures/websocket_requests.json`). Every step is replayed in order against one hidane, with
//! a monitor client connected from the start, and the records each step sends must be the
//! official emulator's, field for field and in the same order, but for:
//!
//! - what changes from run to run: `time` and `requestId` (checked for their form),
//!   `rulesContext.time` (which must be `request.time`), and the number at the end of
//!   `rulesReleaseKey`;
//! - `resource` and `request.resource` where the official emulator's record shows them
//!   undefined: its first pass does not read the stored document, and hidane's only one does;
//! - the order of the fields a list's filter names in `resource.data`, which the official
//!   emulator orders its own way.
//!
//! A step marked `differs` is one where hidane does not do what the official emulator does:
//! intentionally (`docs/parity-exceptions.md`) the records of a second pass, which hidane does
//! not make, a Listen query target checked again, and paths the official emulator does not
//! answer; not yet, the `resource.__name__` a list of a collection group below a document holds.
//! Only the method and path of its records are compared, or for a handshake, that hidane answers
//! `404`. A client that does not read, which holds up the official emulator's requests, is a test
//! of its own.

mod common;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use hidane_proto::google::firestore::v1::{
    ListenRequest, Target,
    firestore_client::FirestoreClient,
    listen_request,
    target::{DocumentsTarget, QueryTarget, TargetType, query_target},
};
use serde_json::{Map, Value as Json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::{Instant, timeout},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

/// The REST and gRPC port, and the WebSocket port.
async fn start() -> (SocketAddr, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let websocket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = websocket.local_addr().unwrap();
    let admin = hidane::Admin::default();
    admin.rules().set_debug_log(Arc::new(|_: &str| {}));
    let monitor = hidane::rules::monitor::Monitor::new();
    admin.rules().monitor_requests(Arc::clone(&monitor));
    tokio::spawn(hidane::serve_websocket(vec![websocket], monitor));
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    (addr, ws_addr)
}

/// The status and body of a REST call.
async fn send(addr: SocketAddr, step: &Json, body: &str) -> (u16, String) {
    let mut headers = String::new();
    if let Some(auth) = step["auth"].as_str() {
        headers.push_str(&format!("Authorization: {auth}\r\n"));
    }
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: \
         application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        step["method"].as_str().unwrap(),
        step["path"].as_str().unwrap(),
        body.len(),
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    (text[9..12].parse().unwrap(), body)
}

/// A WebSocket client of the feed.
struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
    status: String,
    headers: Vec<String>,
    /// Whether text frames come as the text itself, not as the JSON it holds.
    as_text: bool,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /requests HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                  Sec-WebSocket-Version: 13\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        let end = loop {
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            assert_ne!(stream.read_buf(&mut buf).await.unwrap(), 0);
        };
        let head = String::from_utf8(buf[..end].to_vec()).unwrap();
        buf.drain(..end + 4);
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap().to_owned();
        assert!(head.contains("sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        let headers = lines
            .map(|line| line.split_once(": ").unwrap().0.to_owned())
            .collect();
        Self {
            stream,
            buf,
            status,
            headers,
            as_text: false,
        }
    }

    /// The next frame, if one comes before `deadline`: text as JSON, others as their opcode and
    /// payload, and `"EOF"` when the server closed.
    async fn frame(&mut self, deadline: Instant) -> Option<Json> {
        loop {
            if let Some(frame) = self.parse() {
                return Some(frame);
            }
            match tokio::time::timeout_at(deadline, self.stream.read_buf(&mut self.buf)).await {
                Ok(Ok(0) | Err(_)) => return Some(Json::from("EOF")),
                Ok(Ok(_)) => {}
                Err(_) => return None,
            }
        }
    }

    fn parse(&mut self) -> Option<Json> {
        let buf = &self.buf;
        if buf.len() < 2 {
            return None;
        }
        assert_eq!(buf[0] & 0x80, 0x80, "a fragmented message");
        assert_eq!(buf[1] & 0x80, 0, "a masked frame");
        let (length, at) = match buf[1] & 0x7f {
            126 => (
                usize::from(u16::from_be_bytes([*buf.get(2)?, *buf.get(3)?])),
                4,
            ),
            127 => (
                usize::try_from(u64::from_be_bytes(buf.get(2..10)?.try_into().unwrap())).unwrap(),
                10,
            ),
            n => (usize::from(n), 2),
        };
        let payload = buf.get(at..at + length)?.to_vec();
        let opcode = buf[0] & 0x0f;
        self.buf.drain(..at + length);
        Some(if opcode == 1 && self.as_text {
            Json::from(String::from_utf8(payload).unwrap())
        } else if opcode == 1 {
            serde_json::from_slice(&payload).unwrap()
        } else {
            let payload: String = payload.iter().map(|&b| char::from(b)).collect();
            serde_json::json!({"opcode": opcode, "payload": payload})
        })
    }

    /// The frames that come: `count` of them, then any other for a moment; or, without a
    /// count, those that come before a pause.
    async fn frames(&mut self, count: Option<usize>, pause: Duration) -> Vec<Json> {
        let mut frames = Vec::new();
        let end = Instant::now() + Duration::from_secs(10);
        while count.is_some_and(|n| frames.len() < n) {
            match self.frame(end).await {
                Some(frame) if frame == "EOF" => {
                    frames.push(frame);
                    return frames;
                }
                Some(frame) => frames.push(frame),
                None => panic!("{} frames of {count:?} came", frames.len()),
            }
        }
        while let Some(frame) = self.frame(Instant::now() + pause).await {
            let eof = frame == "EOF";
            frames.push(frame);
            if eof {
                break;
            }
        }
        frames
    }

    async fn send(&mut self, opcode: u8, payload: &str) {
        let data: Vec<u8> = payload.chars().map(|c| u8::try_from(c).unwrap()).collect();
        let mask = [7, 1, 3, 5];
        let mut frame = vec![0x80 | opcode, 0x80 | u8::try_from(data.len()).unwrap()];
        frame.extend_from_slice(&mask);
        frame.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame).await.unwrap();
    }
}

/// What the WebSocket port answers to `request`: status line and body.
async fn raw(addr: SocketAddr, request: &str) -> Json {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let _ = timeout(Duration::from_secs(5), async {
        let mut chunk = [0; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
            }
            // The answer to a request that is not a handshake ends with its body.
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..at]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .map_or(0, |n| n.trim().parse().unwrap());
                if raw.len() >= at + 4 + length {
                    break;
                }
            }
        }
    })
    .await;
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    serde_json::json!({"status": head.lines().next().unwrap(), "body": body})
}

fn listen_request(database: &str, target: &Json) -> ListenRequest {
    let target_type = match target.get("documents") {
        Some(documents) => TargetType::Documents(DocumentsTarget {
            documents: documents["documents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap().to_owned())
                .collect(),
        }),
        None => TargetType::Query(QueryTarget {
            parent: target["query"]["parent"].as_str().unwrap().to_owned(),
            query_type: Some(query_target::QueryType::StructuredQuery(
                common::structured_query::structured_query(&target["query"]["structuredQuery"]),
            )),
        }),
    };
    ListenRequest {
        database: database.to_owned(),
        target_change: Some(listen_request::TargetChange::AddTarget(Target {
            target_id: i32::try_from(target["targetId"].as_i64().unwrap()).unwrap(),
            target_type: Some(target_type),
            ..Target::default()
        })),
        ..ListenRequest::default()
    }
}

fn fields(value: &mut Json) -> Option<&mut Map<String, Json>> {
    value
        .get_mut("mapValue")?
        .get_mut("fields")?
        .as_object_mut()
}

/// Checks the record's form, and leaves out what changes from run to run.
fn without_times(record: &mut Json) {
    let is_time = |t: &str| t.len() >= 20 && t.ends_with('Z') && &t[10..11] == "T";
    let map = record.as_object_mut().unwrap();
    let time = map.shift_remove("time").unwrap();
    assert!(is_time(time.as_str().unwrap()), "{time}");
    let id = map.shift_remove("requestId").unwrap();
    let id = id.as_str().unwrap();
    let parts: Vec<usize> = id.split('-').map(str::len).collect();
    assert_eq!(parts, [8, 4, 4, 4, 12], "{id}");
    let key = map["rulesReleaseKey"].as_str().unwrap();
    let key = key.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-');
    map.insert("rulesReleaseKey".to_owned(), Json::from(key));
    let context = map["rulesContext"].as_object_mut().unwrap();
    let time = context.shift_remove("time").unwrap();
    let request = fields(&mut context["request"]).unwrap();
    assert_eq!(
        request.shift_remove("time").unwrap()["timestampValue"],
        time
    );
}

/// `data`'s fields in name order.
fn sort_data(record: &mut Json) {
    let context = &mut record["rulesContext"];
    if context["method"] != "list" {
        return;
    }
    if let Some(data) = fields(&mut context["resource"])
        .and_then(|r| r.get_mut("data"))
        .and_then(fields)
    {
        data.sort_keys();
    }
}

/// The official emulator's record and hidane's, without what is not compared.
fn compared(official: &Json, hidane: &Json) -> (String, String) {
    let (mut official, mut hidane) = (official.clone(), hidane.clone());
    without_times(&mut official);
    without_times(&mut hidane);
    let undefined = serde_json::json!({"undefined": {}});
    if official["rulesContext"]["resource"] == undefined {
        official["rulesContext"]["resource"] = hidane["rulesContext"]["resource"].clone();
    }
    let ours = fields(&mut hidane["rulesContext"]["request"])
        .and_then(|request| request.get("resource").cloned());
    let theirs = fields(&mut official["rulesContext"]["request"]).unwrap();
    if theirs.get("resource") == Some(&undefined)
        && let Some(resource) = ours
    {
        theirs.insert("resource".to_owned(), resource);
    }
    sort_data(&mut official);
    sort_data(&mut hidane);
    (official.to_string(), hidane.to_string())
}

fn same_records(label: &str, official: &[Json], hidane: &[Json]) {
    assert_eq!(official.len(), hidane.len(), "{label}: {hidane:#?}");
    for (official, hidane) in official.iter().zip(hidane) {
        match (official, hidane) {
            (Json::Array(official), Json::Array(hidane)) => {
                same_records(&format!("{label} (backlog)"), official, hidane);
            }
            (Json::Object(o), Json::Object(_)) if o.contains_key("rulesContext") => {
                let (official, hidane) = compared(official, hidane);
                assert_eq!(hidane, official, "{label}");
            }
            _ => assert_eq!(hidane, official, "{label}"),
        }
    }
}

/// For a step marked `differs`: each of hidane's records has a method and path the official
/// emulator recorded, or where it recorded nothing (its monitor stopped), the path of the call.
fn similar_records(label: &str, path: &str, official: &[Json], hidane: &[Json]) {
    let place = |record: &Json| {
        let context = &record["rulesContext"];
        format!("{} {}", context["method"], context["path"])
    };
    let theirs: Vec<String> = official.iter().map(place).collect();
    assert!(!hidane.is_empty(), "{label}");
    for record in hidane {
        if official.is_empty() {
            let theirs = record["rulesContext"]["path"].as_str().unwrap();
            assert!(path.ends_with(theirs), "{label}: {record}");
        } else {
            assert!(theirs.contains(&place(record)), "{label}: {record}");
        }
    }
}

#[tokio::test]
async fn records_as_the_official_emulator_sends_them() {
    replay(include_str!("fixtures/websocket_requests.json")).await;
}

/// The official emulator's monitor sends nothing from a decision that reaches the limit of
/// 1,000 expressions on (`tools/oracle/rules_expressions_monitor.py`); hidane's records it, and
/// every decision after it.
#[tokio::test]
async fn a_decision_past_the_limit_of_expressions() {
    replay(include_str!("fixtures/rules_expressions_monitor.json")).await;
}

/// How the feed writes strings that JSON may escape (`tools/oracle/websocket_escapes.py`), text
/// for text: `"`, `\` and the control characters escaped, U+2028 and U+2029 as `\u2028` and
/// `\u2029`, and the HTML characters, `/`, U+007F, U+0080 and the rest as they are, in the
/// ruleset, the path, keys, values and the token's claims. A record is compared but for what
/// changes from run to run and a `resource` the official emulator's record shows undefined.
#[tokio::test]
async fn strings_written_as_the_official_emulator_writes_them() {
    const UNDEFINED: &str = ",\"resource\":{\"undefined\":{}}}}";
    let cases: Vec<Json> =
        serde_json::from_str(include_str!("fixtures/websocket_escapes.json")).unwrap();
    let (addr, ws_addr) = start().await;
    let mut monitor = Client::connect(ws_addr).await;
    monitor.as_text = true;
    let pause = Duration::from_millis(300);
    assert_eq!(monitor.frames(Some(1), pause).await, ["[]"]);
    for case in &cases {
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let label = format!("{} #{i}", case["name"].as_str().unwrap());
            let (status, _) = send(addr, step, &step["body"].to_string()).await;
            assert_eq!(Json::from(status), step["status"], "{label}");
            let expected = step["frames"].as_array().unwrap();
            let frames = monitor.frames(Some(expected.len()), pause).await;
            assert_eq!(frames.len(), expected.len(), "{label}");
            for (frame, expected) in frames.iter().zip(expected) {
                let (mut frame, mut expected) = (steady(frame), steady(expected));
                if expected.ends_with(UNDEFINED) {
                    for text in [&mut frame, &mut expected] {
                        text.truncate(text.rfind(",\"resource\":").unwrap());
                    }
                }
                assert_eq!(frame, expected, "{label}");
            }
        }
    }
}

/// A record's text without what changes from run to run: the values of `time`, `requestId`
/// and `timestampValue`, and the number at the end of `rulesReleaseKey`.
fn steady(frame: &Json) -> String {
    const KEYS: [&str; 4] = [
        "\"time\":\"",
        "\"requestId\":\"",
        "\"timestampValue\":\"",
        "\"rulesReleaseKey\":\"",
    ];
    let mut rest = frame.as_str().unwrap();
    let mut out = String::with_capacity(rest.len());
    while let Some((at, key)) = KEYS
        .iter()
        .filter_map(|key| rest.find(key).map(|at| (at, *key)))
        .min()
    {
        let start = at + key.len();
        let end = start + rest[start..].find('"').unwrap();
        out.push_str(&rest[..start]);
        if key == KEYS[3] {
            out.push_str(
                rest[start..end].trim_end_matches(|c: char| c.is_ascii_digit() || c == '-'),
            );
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

async fn replay(fixture: &str) {
    let cases: Vec<Json> = serde_json::from_str(fixture).unwrap();
    let (addr, ws_addr) = start().await;
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut grpc = FirestoreClient::new(channel);
    let mut clients: std::collections::HashMap<String, Client> = Default::default();
    let mut transaction: Option<(String, String)> = None;
    let mut listens = Vec::new();
    let pause = Duration::from_millis(50);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let label = format!("{name} #{i}");
            let differs = step.get("differs").is_some();
            if let Some(action) = step["ws"].as_str() {
                let client = step["client"].as_str().unwrap_or_default().to_owned();
                let expected = step["frames"].as_array().cloned().unwrap_or_default();
                let frames = match action {
                    "connect" => {
                        let mut connected = Client::connect(ws_addr).await;
                        assert_eq!(connected.status, step["status"], "{label}");
                        assert_eq!(Json::from(connected.headers.clone()), step["headers"]);
                        let frames = connected.frames(Some(expected.len()), pause).await;
                        clients.insert(client, connected);
                        frames
                    }
                    "raw" => {
                        let answer = raw(ws_addr, step["request"].as_str().unwrap()).await;
                        if differs {
                            assert_eq!(answer["status"], "HTTP/1.1 404 Not Found", "{label}");
                        } else {
                            assert_eq!(answer, step["answer"], "{label}");
                        }
                        continue;
                    }
                    "send" => {
                        let connected = clients.get_mut(&client).unwrap();
                        let opcode = u8::try_from(step["opcode"].as_u64().unwrap()).unwrap();
                        connected
                            .send(opcode, step["payload"].as_str().unwrap())
                            .await;
                        connected.frames(Some(expected.len()), pause).await
                    }
                    "receive" => {
                        let connected = clients.get_mut(&client).unwrap();
                        connected.frames(Some(expected.len()), pause).await
                    }
                    // The official emulator's writes waited for a client that did not read:
                    // `a_client_that_does_not_read_is_disconnected`.
                    "stall" => {
                        assert!(step["waited"].is_number(), "{label}");
                        continue;
                    }
                    other => panic!("{other}"),
                };
                if differs {
                    // The official emulator's monitor had stopped: hidane's still sends.
                    assert!(!frames.is_empty(), "{label}");
                } else {
                    same_records(&label, &expected, &frames);
                }
                continue;
            }
            let monitor = clients.get_mut("monitor").unwrap();
            // Without `records`, the monitor had closed: only the status is compared.
            let expected = step["records"].as_array().cloned().unwrap_or_default();
            let expected = &expected[..];
            let count = (!differs).then_some(expected.len());
            if let Some(database) = step["listen"].as_str() {
                let (requests, received) = mpsc::channel(4);
                let mut responses = grpc
                    .listen(ReceiverStream::new(received))
                    .await
                    .unwrap()
                    .into_inner();
                for target in step["targets"].as_array().unwrap() {
                    requests
                        .send(listen_request(database, target))
                        .await
                        .unwrap();
                }
                tokio::spawn(async move { while let Ok(Some(_)) = responses.message().await {} });
                listens.push(requests);
                let records = monitor.frames(count, Duration::from_millis(300)).await;
                same_records(&label, expected, &records);
                continue;
            }
            let mut body = if step["body"].is_null() {
                String::new()
            } else {
                step["body"].to_string()
            };
            if step.get("transaction").is_some() {
                let (theirs, ours) = transaction.as_ref().unwrap();
                body = body.replace(theirs, ours);
            }
            let (status, response) = send(addr, step, &body).await;
            assert_eq!(status, step["status"], "{label}: {response}");
            if step["path"]
                .as_str()
                .unwrap()
                .ends_with(":beginTransaction")
            {
                let ours: Json = serde_json::from_str(&response).unwrap();
                transaction = Some((
                    step["response"]["transaction"].as_str().unwrap().to_owned(),
                    ours["transaction"].as_str().unwrap().to_owned(),
                ));
            }
            if step.get("records").is_none() {
                continue;
            }
            let pause = if differs {
                Duration::from_millis(300)
            } else {
                pause
            };
            let records = monitor.frames(count, pause).await;
            if differs {
                similar_records(&label, step["path"].as_str().unwrap(), expected, &records);
            } else {
                same_records(&label, expected, &records);
            }
        }
    }
}

/// A client that stops reading never holds up a request (the official emulator's requests wait
/// for it): once more than `CLIENT_BUFFER` bytes of records wait for it, it is disconnected,
/// while a client that reads gets every record.
#[tokio::test]
async fn a_client_that_does_not_read_is_disconnected() {
    const WRITES: usize = 100;
    let (addr, ws_addr) = start().await;
    let content = "rules_version = '2';\nservice cloud.firestore {\n  match \
                   /databases/{database}/documents {\n    match /c/{d} {\n      allow write: if \
                   true;\n    }\n  }\n}\n";
    let rules = serde_json::json!({"rules": {"files": [{"name": "r", "content": content}]}});
    let put =
        serde_json::json!({"method": "PUT", "path": "/emulator/v1/projects/slow:securityRules"});
    assert_eq!(send(addr, &put, &rules.to_string()).await.0, 200);

    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut stalled = socket.connect(ws_addr).await.unwrap();
    stalled
        .write_all(
            b"GET /requests HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
              Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
              Sec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reading = Client::connect(ws_addr).await;
    let backlog = reading.frames(Some(1), Duration::ZERO).await;
    assert_eq!(backlog, [Json::Array(Vec::new())]);
    let counted = tokio::spawn(async move {
        let mut count = 0;
        let end = Instant::now() + Duration::from_secs(60);
        while count < WRITES {
            match reading.frame(end).await {
                Some(frame) if frame.is_object() => count += 1,
                _ => break,
            }
        }
        count
    });

    let big = "x".repeat(256 * 1024);
    let body = serde_json::json!({"fields": {"s": {"stringValue": big}}}).to_string();
    for i in 0..WRITES {
        let patch = serde_json::json!({
            "method": "PATCH",
            "path": format!("/v1/projects/slow/databases/(default)/documents/c/{}", i % 10),
        });
        let sent = timeout(Duration::from_secs(10), send(addr, &patch, &body)).await;
        assert_eq!(sent.expect("a write waited").0, 200);
    }
    assert_eq!(counted.await.unwrap(), WRITES);
    // What the stalled client was sent before it was dropped, then the end of the connection.
    let drained = timeout(Duration::from_secs(10), async {
        let mut sink = vec![0; 1 << 16];
        while stalled.read(&mut sink).await.is_ok_and(|n| n > 0) {}
    })
    .await;
    assert!(drained.is_ok(), "the stalled client is still connected");
}
