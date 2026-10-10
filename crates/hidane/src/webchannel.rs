//! WebChannel (`VER=8`): how firebase-js-sdk in the browser carries the Listen and Write streams
//! over HTTP/1.1, served as the official emulator v1.22.0 serves it (`docs/webchannel.md`).
//!
//! - Handshake: `POST /google.firestore.v1.Firestore/{Listen,Write}/channel?VER=8&RID=…` with a
//!   form body: `headers` (the request's headers, `Name:Value` lines), `count`, `ofs` and
//!   `req{i}___data__`, the first messages in ProtoJSON. It creates a session and answers
//!   `[[0,["c","<SID>","",8,12,30000]]]`.
//! - Back channel: `GET …?RID=rpc&SID=…&AID=<last id received>&CI=…&TYPE=xmlhttp` sends the
//!   server's messages as numbered arrays, `[[id,[message]],…]`; a `noop` when it opens and
//!   every 30 s, and the response ends after 60 s, when the client opens another. With `CI=1`
//!   (long polling) it ends with the first data.
//! - Forward channel: `POST …?SID=…&RID=…&AID=…` with more messages (`ofs` numbers them) is
//!   answered `[<1 with a back channel open, else 0>,<last id>,7]`.
//! - `POST …?SID=…&TYPE=terminate` ends the session; an unknown session is `400` with no body.
//! - A stream's error is a message `{"error":{"message":"STATUS: …","status":"STATUS"}}`.
//!
//! Every answer is `<length>\n<JSON>`, the length in UTF-16 code units, as the browser counts.

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    hash::{BuildHasher, Hasher},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use hidane_proto::google::firestore::v1::{ListenRequest, WriteRequest};
use prost_reflect::{DynamicMessage, MessageDescriptor};
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, sleep_until},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use crate::{
    firestore::{FirestoreService, auth},
    rest,
};

const NOOP_INTERVAL: Duration = Duration::from_secs(30);
const BACK_CHANNEL_LIFETIME: Duration = Duration::from_secs(60);
/// Sessions without a back channel or a request for this long are dropped (the official
/// emulator keeps them, firebase-tools #11124).
const IDLE_SESSION: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Listen,
    Write,
}

impl Kind {
    pub fn from_rpc(rpc: &str) -> Option<Self> {
        match rpc {
            "Listen" => Some(Self::Listen),
            "Write" => Some(Self::Write),
            _ => None,
        }
    }
}

/// The open sessions.
#[derive(Default)]
pub struct Channels {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    sweeping: AtomicBool,
}

struct Session {
    kind: Kind,
    /// The stream's requests; taken when the session ends, which ends the stream.
    requests: Mutex<Option<Requests>>,
    state: Mutex<State>,
    /// The id of the last message queued, so back channels wake up for it.
    latest: watch::Sender<u64>,
}

enum Requests {
    Listen(mpsc::Sender<Result<ListenRequest, Status>>),
    Write(mpsc::Sender<Result<WriteRequest, Status>>),
}

struct State {
    last_id: u64,
    /// Messages the client has not acknowledged (with an `AID` at or past their id), each
    /// rendered `[id,[…]]`.
    outbox: VecDeque<(u64, String)>,
    /// The offset of the next client message, so a retried POST is not delivered twice.
    next_ofs: u64,
    /// The generation of the open back channel, 0 when none is open.
    back_channel: u64,
    generations: u64,
    last_seen: Instant,
    closed: bool,
}

impl Session {
    fn push(&self, payload: &str) -> u64 {
        let id = {
            let mut state = self.state.lock().expect("session lock");
            state.last_id += 1;
            let id = state.last_id;
            state.outbox.push_back((id, format!("[{id},[{payload}]]")));
            id
        };
        self.latest.send_replace(id);
        id
    }

    /// The queued messages after `after`, as one array, and the last id in it.
    fn after(&self, after: u64) -> Option<(String, u64)> {
        let state = self.state.lock().expect("session lock");
        let entries: Vec<&str> = state
            .outbox
            .iter()
            .filter(|(id, _)| *id > after)
            .map(|(_, entry)| entry.as_str())
            .collect();
        (!entries.is_empty()).then(|| (format!("[{}]", entries.join(",")), state.last_id))
    }

    /// The client has every message up to `aid`.
    fn acknowledge(&self, aid: u64) {
        let mut state = self.state.lock().expect("session lock");
        while state.outbox.front().is_some_and(|(id, _)| *id <= aid) {
            state.outbox.pop_front();
        }
        state.last_seen = Instant::now();
    }

    fn attach(&self) -> u64 {
        let generation = {
            let mut state = self.state.lock().expect("session lock");
            state.generations += 1;
            state.back_channel = state.generations;
            state.last_seen = Instant::now();
            state.generations
        };
        // An older back channel wakes up and sees it is replaced.
        self.latest.send_modify(|_| {});
        generation
    }

    fn detach(&self, generation: u64) {
        let mut state = self.state.lock().expect("session lock");
        if state.back_channel == generation {
            state.back_channel = 0;
            state.last_seen = Instant::now();
        }
    }

    fn current(&self, generation: u64) -> bool {
        let state = self.state.lock().expect("session lock");
        state.back_channel == generation && !state.closed
    }

    fn close(&self) {
        self.requests.lock().expect("session lock").take();
        self.state.lock().expect("session lock").closed = true;
        self.latest.send_modify(|_| {});
    }

    /// Delivers the client's messages `ofs`, `ofs + 1`, … once each, in order.
    async fn deliver(&self, ofs: u64, messages: &[String]) -> Result<(), ()> {
        for (i, json) in messages.iter().enumerate() {
            let position = ofs + i as u64;
            {
                let mut state = self.state.lock().expect("session lock");
                if position < state.next_ofs {
                    continue;
                }
                state.next_ofs = position + 1;
                state.last_seen = Instant::now();
            }
            let requests = match &*self.requests.lock().expect("session lock") {
                Some(Requests::Listen(tx)) => Some(Requests::Listen(tx.clone())),
                Some(Requests::Write(tx)) => Some(Requests::Write(tx.clone())),
                None => None,
            };
            match requests {
                Some(Requests::Listen(tx)) => {
                    let request = parse::<ListenRequest>(json, "ListenRequest")?;
                    let _ = tx.send(Ok(request)).await;
                }
                Some(Requests::Write(tx)) => {
                    let request = parse::<WriteRequest>(json, "WriteRequest")?;
                    let _ = tx.send(Ok(request)).await;
                }
                None => {}
            }
        }
        Ok(())
    }
}

fn parse<T: prost::Message + Default>(json: &str, name: &str) -> Result<T, ()> {
    let mut deserializer = serde_json::Deserializer::from_str(json);
    DynamicMessage::deserialize(rest::descriptor(name), &mut deserializer)
        .ok()
        .and_then(|message| message.transcode_to().ok())
        .ok_or(())
}

/// ProtoJSON of a response, compact.
fn to_json<T: prost::Message>(message: &T, descriptor: &MessageDescriptor) -> String {
    let dynamic = DynamicMessage::decode(descriptor.clone(), message.encode_to_vec().as_slice())
        .expect("a message of its own type");
    serde_json::to_string(&dynamic).expect("ProtoJSON")
}

fn error_json(status: &Status) -> String {
    let (_, name) = rest::http_status(status.code());
    serde_json::json!({"error": {"message": format!("{name}: {}", status.message()), "status": name}})
        .to_string()
}

/// A chunk of the WebChannel wire: the length in UTF-16 code units, a newline, the JSON.
fn frame(json: &str) -> Bytes {
    Bytes::from(format!("{}\n{json}", json.encode_utf16().count()))
}

fn base(status: StatusCode) -> axum::http::response::Builder {
    Response::builder()
        .status(status)
        .header(
            "access-control-expose-headers",
            HeaderValue::from_static(
                "x-client-wire-protocol, x-http-session-id, x-http-initial-response",
            ),
        )
        .header(header::VARY, "Origin")
}

fn text(json: &str) -> Response {
    base(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(frame(json)))
        .expect("a valid response")
}

/// The official emulator's answer to an unknown session or a request it cannot read.
fn bad_request() -> Response {
    base(StatusCode::BAD_REQUEST)
        .body(Body::empty())
        .expect("a valid response")
}

/// A session ID like the official emulator's: 16 random bytes, URL-safe base64.
fn session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let random = std::collections::hash_map::RandomState::new();
    let mut bytes = Vec::with_capacity(16);
    for _ in 0..2 {
        let mut hasher = random.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        bytes.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    URL_SAFE.encode(bytes)
}

fn param<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The client's messages in a form body: their first offset and their JSON.
fn messages(form: &[(String, String)]) -> (u64, Vec<String>) {
    let count: usize = param(form, "count")
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let ofs = param(form, "ofs").and_then(|o| o.parse().ok()).unwrap_or(0);
    let messages = (0..count)
        .filter_map(|i| param(form, &format!("req{i}___data__")).map(str::to_owned))
        .collect();
    (ofs, messages)
}

impl Channels {
    fn get(&self, sid: Option<&str>) -> Option<Arc<Session>> {
        let sid = sid?;
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(sid)
            .cloned()
    }

    /// `POST …/channel`: a handshake, the forward channel or `TYPE=terminate`.
    pub async fn post(
        self: &Arc<Self>,
        service: FirestoreService,
        kind: Kind,
        query: &str,
        body: &[u8],
    ) -> Response {
        let query = rest::parse_query(query);
        let form = rest::parse_query(&String::from_utf8_lossy(body));
        match (param(&query, "SID"), param(&query, "TYPE")) {
            (None, _) => self.handshake(service, kind, &form).await,
            (Some(sid), Some("terminate")) => {
                let session = self.sessions.lock().expect("sessions lock").remove(sid);
                let Some(session) = session else {
                    return bad_request();
                };
                session.close();
                // As on the official emulator, whose header name lacks its hyphen.
                base(StatusCode::OK)
                    .header("contenttype", "text/plain; charset=utf-8")
                    .body(Body::empty())
                    .expect("a valid response")
            }
            (Some(sid), _) => {
                let Some(session) = self.get(Some(sid)).filter(|s| s.kind == kind) else {
                    return bad_request();
                };
                if let Some(aid) = param(&query, "AID").and_then(|a| a.parse().ok()) {
                    session.acknowledge(aid);
                }
                // What the client is told is the state before its messages are delivered.
                let (open, last) = {
                    let state = session.state.lock().expect("session lock");
                    (u8::from(state.back_channel != 0), state.last_id)
                };
                let (ofs, messages) = messages(&form);
                if session.deliver(ofs, &messages).await.is_err() {
                    return bad_request();
                }
                text(&format!("[{open},{last},7]"))
            }
        }
    }

    async fn handshake(
        self: &Arc<Self>,
        service: FirestoreService,
        kind: Kind,
        form: &[(String, String)],
    ) -> Response {
        let authorization = param(form, "headers").and_then(|headers| {
            headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_owned())
            })
        });
        let (latest, _) = watch::channel(0);
        let session = Arc::new(Session {
            kind,
            requests: Mutex::new(None),
            state: Mutex::new(State {
                last_id: 0,
                outbox: VecDeque::new(),
                next_ofs: 0,
                back_channel: 0,
                generations: 0,
                last_seen: Instant::now(),
                closed: false,
            }),
            latest,
        });
        // The header is read when the stream opens, as for gRPC; a bad one is the stream's
        // error.
        match auth::from_header(authorization.as_deref()) {
            Ok(_) => start(&service, kind, &session),
            Err(status) => {
                session.push(&error_json(&status));
            }
        }
        let (ofs, messages) = messages(form);
        if session.deliver(ofs, &messages).await.is_err() {
            session.close();
            return bad_request();
        }
        let sid = session_id();
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(sid.clone(), session);
        self.sweep();
        text(&format!("[[0,[\"c\",\"{sid}\",\"\",8,12,30000]]]"))
    }

    /// `GET …/channel?TYPE=xmlhttp`: the back channel.
    pub fn get_back_channel(&self, kind: Kind, query: &str) -> Response {
        let query = rest::parse_query(query);
        let Some(session) = self.get(param(&query, "SID")).filter(|s| s.kind == kind) else {
            return bad_request();
        };
        let aid = param(&query, "AID")
            .and_then(|a| a.parse().ok())
            .unwrap_or(0);
        let long_poll = param(&query, "CI") == Some("1");
        session.acknowledge(aid);
        let generation = session.attach();
        let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(16);
        tokio::spawn(async move {
            back_channel(&session, generation, aid, long_poll, &tx).await;
            session.detach(generation);
        });
        base(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header("x-content-type-options", "nosniff")
            .header(header::CACHE_CONTROL, "private, max-age=0")
            .body(Body::from_stream(ReceiverStream::new(rx)))
            .expect("a valid response")
    }

    /// Drops idle sessions from time to time, once a session exists.
    fn sweep(self: &Arc<Self>) {
        if self.sweeping.swap(true, Ordering::Relaxed) {
            return;
        }
        let channels: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let Some(channels) = channels.upgrade() else {
                    return;
                };
                let now = Instant::now();
                let idle: Vec<Arc<Session>> = {
                    let mut sessions = channels.sessions.lock().expect("sessions lock");
                    let ids: Vec<String> = sessions
                        .iter()
                        .filter(|(_, s)| {
                            let state = s.state.lock().expect("session lock");
                            state.back_channel == 0 && now - state.last_seen > IDLE_SESSION
                        })
                        .map(|(id, _)| id.clone())
                        .collect();
                    ids.iter().filter_map(|id| sessions.remove(id)).collect()
                };
                for session in idle {
                    session.close();
                }
            }
        });
    }
}

/// Runs `kind`'s stream for `session`, its answers queued as numbered messages.
fn start(service: &FirestoreService, kind: Kind, session: &Arc<Session>) {
    match kind {
        Kind::Listen => {
            let (requests, rx) = mpsc::channel(64);
            let (responses, answers) = mpsc::channel(256);
            *session.requests.lock().expect("session lock") = Some(Requests::Listen(requests));
            let service = service.clone();
            tokio::spawn(async move {
                let mut rx = ReceiverStream::new(rx);
                if let Err(status) = service.serve_listen(&mut rx, &responses).await {
                    let _ = responses.send(Err(status)).await;
                }
            });
            tokio::spawn(pump(answers, Arc::clone(session), "ListenResponse"));
        }
        Kind::Write => {
            let (requests, rx) = mpsc::channel(64);
            let (responses, answers) = mpsc::channel(256);
            *session.requests.lock().expect("session lock") = Some(Requests::Write(requests));
            let service = service.clone();
            tokio::spawn(async move {
                let mut rx = ReceiverStream::new(rx);
                if let Err(status) = service.serve_write_stream(&mut rx, &responses).await {
                    let _ = responses.send(Err(status)).await;
                }
            });
            tokio::spawn(pump(answers, Arc::clone(session), "WriteResponse"));
        }
    }
}

async fn pump<T: prost::Message>(
    mut answers: mpsc::Receiver<Result<T, Status>>,
    session: Arc<Session>,
    name: &str,
) {
    let descriptor = rest::descriptor(name);
    while let Some(answer) = answers.recv().await {
        let json = match answer {
            Ok(message) => to_json(&message, &descriptor),
            Err(status) => error_json(&status),
        };
        session.push(&json);
    }
}

/// Sends `session`'s messages after `aid` to one back channel until it ends: after 60 s, when
/// another opens, when the session ends or the client goes away; with `long_poll`, after the
/// first data.
async fn back_channel(
    session: &Session,
    generation: u64,
    aid: u64,
    long_poll: bool,
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
) {
    let mut latest = session.latest.subscribe();
    let mut sent = aid;
    if long_poll && session.after(sent).is_some() {
        let _ = flush(session, &mut sent, tx).await;
        return;
    }
    if !flush(session, &mut sent, tx).await {
        return;
    }
    session.push("\"noop\"");
    if !flush(session, &mut sent, tx).await {
        return;
    }
    let opened = Instant::now();
    let mut next_noop = opened + NOOP_INTERVAL;
    loop {
        tokio::select! {
            changed = latest.changed() => {
                if changed.is_err() || !session.current(generation) {
                    return;
                }
                let had_data = session.after(sent).is_some();
                if !flush(session, &mut sent, tx).await || (long_poll && had_data) {
                    return;
                }
            }
            () = sleep_until(next_noop) => {
                session.push("\"noop\"");
                if !flush(session, &mut sent, tx).await
                    || long_poll
                    || Instant::now() >= opened + BACK_CHANNEL_LIFETIME
                {
                    return;
                }
                next_noop += NOOP_INTERVAL;
            }
            () = tx.closed() => return,
        }
    }
}

/// Sends what is queued after `sent`; false when the client is gone.
async fn flush(
    session: &Session,
    sent: &mut u64,
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
) -> bool {
    match session.after(*sent) {
        Some((json, last)) => {
            *sent = last;
            tx.send(Ok(frame(&json))).await.is_ok()
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use hidane_core::store::MemoryStore;
    use http_body_util::BodyExt as _;
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    use serde_json::{Value as Json, json};

    use super::{Channels, Kind};
    use crate::firestore::FirestoreService;

    /// The next frame of a body, as `(id, payload)` pairs; `None` when the body ends.
    async fn next(body: &mut Body) -> Option<Vec<(u64, Json)>> {
        let frame = body.frame().await?.unwrap().into_data().unwrap();
        let text = std::str::from_utf8(&frame).unwrap();
        let (_, json) = text.split_once('\n').unwrap();
        let json: Json = serde_json::from_str(json).unwrap();
        Some(
            json.as_array()
                .unwrap()
                .iter()
                .map(|e| (e[0].as_u64().unwrap(), e[1].clone()))
                .collect(),
        )
    }

    /// A `noop` when the back channel opens and every 30 s; it ends after 60 s (measured on
    /// the official emulator, docs/webchannel.md).
    #[tokio::test(start_paused = true)]
    async fn keepalive_and_lifetime() {
        let channels = Arc::new(Channels::default());
        let service = FirestoreService::new(Arc::new(MemoryStore::new()));
        let database = "projects/p/databases/(default)";
        let target = json!({"database": database, "addTarget": {"documents": {"documents": [format!("{database}/documents/c/a")]}, "targetId": 2}});
        let form = format!(
            "count=1&ofs=0&req0___data__={}",
            utf8_percent_encode(&target.to_string(), NON_ALPHANUMERIC)
        );
        let handshake = channels
            .post(service, Kind::Listen, "VER=8&RID=1", form.as_bytes())
            .await;
        let mut body = handshake.into_body();
        let created = next(&mut body).await.unwrap();
        let sid = created[0].1[1].as_str().unwrap().to_owned();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let response =
            channels.get_back_channel(Kind::Listen, &format!("SID={sid}&AID=0&CI=0&TYPE=xmlhttp"));
        let mut body = response.into_body();
        let first = next(&mut body).await.unwrap();
        assert_eq!(
            first.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(next(&mut body).await.unwrap(), [(5, json!(["noop"]))]);
        let opened = tokio::time::Instant::now();
        assert_eq!(next(&mut body).await.unwrap(), [(6, json!(["noop"]))]);
        assert_eq!(opened.elapsed().as_secs(), 30);
        assert_eq!(next(&mut body).await.unwrap(), [(7, json!(["noop"]))]);
        assert_eq!(opened.elapsed().as_secs(), 60);
        assert!(next(&mut body).await.is_none(), "the back channel ends");
    }
}
