//! WebChannel, the browser SDK's transport, against the official emulator's recordings
//! (`fixtures/webchannel.json`, from `tools/oracle/webchannel.py`): handshakes, back channels
//! (streaming and long polling), the forward channel, errors, terminate and unknown sessions.
//!
//! Long-polling reads are compared on their first data and whether the response ended: where
//! the official emulator puts its `noop`s there depends on timing.

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;

const DATABASE: &str = "projects/webchannel/databases/(default)";
const HEADERS: &str =
    "X-Goog-Api-Client:gl-js/ fire/13.0.0\r\nContent-Type:text/plain\r\nx-goog-api-key:demo\r\n";

type HttpClient = Client<HttpConnector, Full<Bytes>>;

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

fn encode(text: &str) -> String {
    utf8_percent_encode(text, NON_ALPHANUMERIC).to_string()
}

fn query(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `<length>\n<JSON>` frames from a response, the length in UTF-16 code units.
struct Frames {
    body: Incoming,
    buffer: Vec<u8>,
    ended: bool,
}

impl Frames {
    fn parse(&mut self) -> Option<Json> {
        let newline = self.buffer.iter().position(|&b| b == b'\n')?;
        let length: usize = std::str::from_utf8(&self.buffer[..newline])
            .unwrap()
            .parse()
            .unwrap();
        let (json, consumed) = {
            let rest = &self.buffer[newline + 1..];
            let text = match std::str::from_utf8(rest) {
                Ok(text) => text,
                Err(e) => std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap(),
            };
            let (mut units, mut end) = (0, 0);
            for c in text.chars() {
                if units == length {
                    break;
                }
                units += c.len_utf16();
                end += c.len_utf8();
            }
            if units != length {
                return None;
            }
            let json: Json = serde_json::from_str(&text[..end]).unwrap();
            (json, newline + 1 + end)
        };
        self.buffer.drain(..consumed);
        Some(json)
    }

    async fn next(&mut self) -> Option<Json> {
        loop {
            if let Some(json) = self.parse() {
                return Some(json);
            }
            if self.ended {
                return None;
            }
            match tokio::time::timeout(Duration::from_secs(10), self.body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.buffer.extend_from_slice(&data);
                    }
                }
                Ok(_) => self.ended = true,
                Err(_) => return None,
            }
        }
    }
}

/// The oracle's placeholders for server times, tokens, stream IDs and long strings.
fn mask(value: &Json) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if k.ends_with("Time") {
                        json!("<time>")
                    } else if k == "resumeToken" || k == "streamId" {
                        json!(format!("<{k}>"))
                    } else {
                        mask(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Json::Array(values) => Json::Array(values.iter().map(mask).collect()),
        Json::String(s) if s.chars().count() > 64 => {
            json!(format!("<{} characters>", s.chars().count()))
        }
        other => other.clone(),
    }
}

struct Session {
    rpc: String,
    sid: String,
    rid: u64,
    aid: u64,
    ofs: u64,
    long_poll: bool,
    frames: Option<Frames>,
}

fn form(messages: &[Json], ofs: u64, headers: Option<&str>) -> String {
    let mut fields = Vec::new();
    if let Some(headers) = headers {
        fields.push(format!("headers={}", encode(headers)));
    }
    fields.push(format!("count={}", messages.len()));
    fields.push(format!("ofs={ofs}"));
    for (i, m) in messages.iter().enumerate() {
        fields.push(format!("req{i}___data__={}", encode(&m.to_string())));
    }
    fields.join("&")
}

async fn send(
    client: &HttpClient,
    method: &str,
    url: String,
    body: Option<(String, &str)>,
) -> hyper::Response<Incoming> {
    let builder = hyper::Request::builder().method(method).uri(url);
    let request = match body {
        Some((body, content_type)) => builder
            .header("content-type", content_type)
            .body(Full::new(Bytes::from(body))),
        None => builder.body(Full::new(Bytes::new())),
    }
    .unwrap();
    client.request(request).await.unwrap()
}

/// What a long-polling read is compared on.
fn long_poll_view(outcome: &Json) -> Json {
    let first = outcome["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry[1].clone())
        .find(|payload| payload != &json!(["noop"]));
    json!({"first": first, "ended": outcome["ended"]})
}

#[tokio::test]
async fn webchannel_like_the_official_emulator() {
    let fixture: Json = serde_json::from_str(include_str!("fixtures/webchannel.json")).unwrap();
    let addr = start().await;
    let client: HttpClient = Client::builder(TokioExecutor::new()).build_http();
    let base = format!("http://{addr}");
    let path = |rpc: &str| format!("{base}/google.firestore.v1.Firestore/{rpc}/channel");
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut differences = Vec::new();
    for step in fixture["steps"].as_array().unwrap() {
        let name = step["session"].as_str().unwrap_or_default().to_owned();
        let messages = step["messages"].as_array().cloned().unwrap_or_default();
        let (actual, expected) = match step["op"].as_str().unwrap() {
            "handshake" => {
                let rpc = step["rpc"].as_str().unwrap().to_owned();
                let url = format!(
                    "{}?{}",
                    path(&rpc),
                    query(&[
                        ("VER", "8".into()),
                        ("database", DATABASE.into()),
                        ("RID", "1000".into()),
                        ("CVER", "22".into()),
                        ("X-HTTP-Session-Id", "gsessionid".into()),
                        ("zx", "x".into()),
                        ("t", "1".into())
                    ])
                );
                let response = send(
                    &client,
                    "POST",
                    url,
                    Some((
                        form(&messages, 0, Some(HEADERS)),
                        "application/x-www-form-urlencoded",
                    )),
                )
                .await;
                let status = response.status().as_u16();
                let mut frames = Frames {
                    body: response.into_body(),
                    buffer: Vec::new(),
                    ended: false,
                };
                let mut frame = frames.next().await.unwrap();
                let sid = frame[0][1][1].as_str().unwrap().to_owned();
                frame[0][1][1] = json!("<sid>");
                sessions.insert(
                    name.clone(),
                    Session {
                        rpc,
                        sid,
                        rid: 1001,
                        aid: 0,
                        ofs: messages.len() as u64,
                        long_poll: false,
                        frames: None,
                    },
                );
                tokio::time::sleep(Duration::from_millis(300)).await;
                (
                    json!({"status": status, "frame": frame}),
                    step["outcome"].clone(),
                )
            }
            "open" => {
                let s = sessions.get_mut(&name).unwrap();
                let ci = step["ci"].as_u64().unwrap();
                s.long_poll = ci == 1;
                let url = format!(
                    "{}?{}",
                    path(&s.rpc),
                    query(&[
                        ("VER", "8".into()),
                        ("database", DATABASE.into()),
                        ("RID", "rpc".into()),
                        ("SID", s.sid.clone()),
                        ("AID", s.aid.to_string()),
                        ("CI", ci.to_string()),
                        ("TYPE", "xmlhttp".into()),
                        ("zx", "x".into()),
                        ("t", "1".into())
                    ])
                );
                let response = send(&client, "GET", url, None).await;
                let status = response.status().as_u16();
                s.frames = Some(Frames {
                    body: response.into_body(),
                    buffer: Vec::new(),
                    ended: false,
                });
                (json!({"status": status}), step["outcome"].clone())
            }
            "read" => {
                let s = sessions.get_mut(&name).unwrap();
                let until = step["until"].as_str().unwrap();
                let frames = s.frames.as_mut().unwrap();
                let mut read = Vec::new();
                while let Some(frame) = frames.next().await {
                    for entry in frame.as_array().unwrap() {
                        s.aid = entry[0].as_u64().unwrap();
                        read.push(mask(entry));
                    }
                    let last = frame.to_string();
                    let done = match until {
                        "end" => false,
                        "noop" => last.contains("\"noop\""),
                        other => last.contains(other),
                    };
                    if done {
                        break;
                    }
                }
                let actual = json!({"messages": read, "ended": frames.ended});
                if s.long_poll {
                    (long_poll_view(&actual), long_poll_view(&step["outcome"]))
                } else {
                    (actual, step["outcome"].clone())
                }
            }
            "forward" => {
                let s = sessions.get_mut(&name).unwrap();
                let url = format!(
                    "{}?{}",
                    path(&s.rpc),
                    query(&[
                        ("VER", "8".into()),
                        ("database", DATABASE.into()),
                        ("SID", s.sid.clone()),
                        ("RID", s.rid.to_string()),
                        ("AID", s.aid.to_string()),
                        ("zx", "x".into()),
                        ("t", "1".into())
                    ])
                );
                let body = form(&messages, s.ofs, None);
                s.ofs += messages.len() as u64;
                s.rid += 1;
                let response = send(
                    &client,
                    "POST",
                    url,
                    Some((body, "application/x-www-form-urlencoded")),
                )
                .await;
                let status = response.status().as_u16();
                let mut frames = Frames {
                    body: response.into_body(),
                    buffer: Vec::new(),
                    ended: false,
                };
                let frame = frames.next().await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                (
                    json!({"status": status, "frame": frame}),
                    step["outcome"].clone(),
                )
            }
            "terminate" => {
                let s = sessions.get(&name).unwrap();
                let url = format!(
                    "{}?{}",
                    path(&s.rpc),
                    query(&[
                        ("VER", "8".into()),
                        ("database", DATABASE.into()),
                        ("SID", s.sid.clone()),
                        ("RID", s.rid.to_string()),
                        ("TYPE", "terminate".into()),
                        ("zx", "x".into())
                    ])
                );
                let response = send(
                    &client,
                    "POST",
                    url,
                    Some((String::new(), "text/plain;charset=UTF-8")),
                )
                .await;
                let status = response.status().as_u16();
                let body = response.into_body().collect().await.unwrap().to_bytes();
                (
                    json!({"status": status, "body": String::from_utf8_lossy(&body)}),
                    step["outcome"].clone(),
                )
            }
            "unknown" => {
                let rpc = step["rpc"].as_str().unwrap();
                let mut params = vec![
                    ("VER", "8".to_owned()),
                    ("database", DATABASE.to_owned()),
                    ("SID", "no-such-session".to_owned()),
                    ("RID", "5".to_owned()),
                    ("AID", "0".to_owned()),
                    ("zx", "x".to_owned()),
                ];
                let method = match step["method"].as_str().unwrap() {
                    "GET" => {
                        params[3] = ("RID", "rpc".to_owned());
                        params.extend([("CI", "0".to_owned()), ("TYPE", "xmlhttp".to_owned())]);
                        "GET"
                    }
                    "terminate" => {
                        params.push(("TYPE", "terminate".to_owned()));
                        "POST"
                    }
                    _ => "POST",
                };
                let url = format!("{}?{}", path(rpc), query(&params));
                let body = (method == "POST").then(|| {
                    (
                        "count=0&ofs=0".to_owned(),
                        "application/x-www-form-urlencoded",
                    )
                });
                let response = send(&client, method, url, body).await;
                let status = response.status().as_u16();
                let body = response.into_body().collect().await.unwrap().to_bytes();
                (
                    json!({"status": status, "body": String::from_utf8_lossy(&body)}),
                    step["outcome"].clone(),
                )
            }
            "commit" => {
                let body = json!({"writes": step["writes"]}).to_string();
                let request = hyper::Request::builder()
                    .method("POST")
                    .uri(format!("{base}/v1/{DATABASE}/documents:commit"))
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer owner")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
                let response = client.request(request).await.unwrap();
                let status = response.status().as_u16();
                response.into_body().collect().await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                (json!({"status": status}), step["outcome"].clone())
            }
            other => panic!("unknown step {other}"),
        };
        if actual != expected {
            differences.push(format!(
                "{} {name}:\n    official {expected}\n    hidane   {actual}",
                step["op"].as_str().unwrap()
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// A handshake for a Listen session on `c/a`; its session ID.
async fn listen_session(client: &HttpClient, base: &str) -> String {
    let url = format!(
        "{base}/google.firestore.v1.Firestore/Listen/channel?{}",
        query(&[
            ("VER", "8".into()),
            ("database", DATABASE.into()),
            ("RID", "1".into()),
            ("CVER", "22".into())
        ])
    );
    let target = json!({"database": DATABASE, "addTarget": {"documents": {"documents": [format!("{DATABASE}/documents/c/a")]}, "targetId": 2}});
    let response = send(
        client,
        "POST",
        url,
        Some((
            form(&[target], 0, Some(HEADERS)),
            "application/x-www-form-urlencoded",
        )),
    )
    .await;
    let mut frames = Frames {
        body: response.into_body(),
        buffer: Vec::new(),
        ended: false,
    };
    frames.next().await.unwrap()[0][1][1]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn back_channel(client: &HttpClient, base: &str, sid: &str, aid: u64) -> Frames {
    let url = format!(
        "{base}/google.firestore.v1.Firestore/Listen/channel?{}",
        query(&[
            ("VER", "8".into()),
            ("database", DATABASE.into()),
            ("RID", "rpc".into()),
            ("SID", sid.into()),
            ("AID", aid.to_string()),
            ("CI", "0".into()),
            ("TYPE", "xmlhttp".into())
        ])
    );
    let response = send(client, "GET", url, None).await;
    Frames {
        body: response.into_body(),
        buffer: Vec::new(),
        ended: false,
    }
}

/// Ids and payloads of a frame.
fn entries(frame: &Json) -> Vec<(u64, Json)> {
    frame
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_u64().unwrap(), e[1].clone()))
        .collect()
}

/// A back channel opened with an older `AID` gets what came after it again, and replaces the
/// one that was open.
#[tokio::test]
async fn reopened_back_channel_resends_and_replaces() {
    let addr = start().await;
    let client: HttpClient = Client::builder(TokioExecutor::new()).build_http();
    let base = format!("http://{addr}");
    let sid = listen_session(&client, &base).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut first = back_channel(&client, &base, &sid, 0).await;
    let initial = entries(&first.next().await.unwrap());
    assert_eq!(initial.first().unwrap().0, 1);
    let mut second = back_channel(&client, &base, &sid, 2).await;
    let resent = entries(&second.next().await.unwrap());
    assert_eq!(
        resent.first().unwrap().0,
        3,
        "messages after AID 2 come again"
    );
    assert_eq!(resent[0].1, initial[2].1);
    while first.next().await.is_some() {}
    assert!(first.ended, "the replaced back channel ends");
}
