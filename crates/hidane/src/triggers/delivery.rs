//! Sending events to the Functions emulator (`--functions_emulator`), as the official emulator
//! does (`tests/fixtures/function_events.json`, from `tools/oracle/function_events.py`).
//!
//! Each event is a `POST /functions/projects/{p}/triggers/{key}`, where firebase-tools runs the
//! function the key names. Events leave one at a time, in the order of the commits, from a
//! task of their own: a write never waits for them. The next event waits for the answer to the
//! last, for up to 20 seconds (the official emulator's HTTP client gives up then too). An answer
//! that is not `2xx`, or a request that fails, is a `WARNING` with the official emulator's
//! wording, and the event is not sent again. Without `--functions_emulator`, every event is a
//! `WARNING` instead.
//!
//! - v1 events are `application/json`;
//! - v2 events are binary-mode CloudEvents: `ce-*` headers, and the data in protobuf, which
//!   firebase-tools expects in base64 (`functionsEmulator.js` decodes it when the content type is
//!   `application/protobuf`). `ce-time` is when the event leaves, to the second; the
//!   `withAuthContext` types carry the official emulator's `ce-authtype: unknown` and
//!   `ce-authid: fake-auth-id@gmail.com`, whoever wrote. `ce-source` is
//!   `//firestore.googleapis.com/projects/{p}/databases/{d}`, as in production: the official
//!   emulator writes `projects/` twice (docs/parity-exceptions.md).

use std::{
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{HeaderName, HeaderValue, StatusCode, header},
};
use hyper_util::rt::TokioIo;
use tokio::{
    net::TcpStream,
    sync::{Notify, mpsc},
    time::timeout,
};

use super::{Log, events::uuid};
use crate::rest;

/// How long a connection, and then an answer, may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The most of an answer's body kept for its `WARNING`.
const MAX_ANSWER: usize = 64 * 1024;

/// Events waiting to leave beyond this many are dropped, with a `WARNING`, so a Functions
/// emulator that stopped answering cannot make the queue grow without end.
const MAX_PENDING: usize = 100_000;

const DATA_SCHEMA: &str = "https://github.com/googleapis/google-cloudevents/blob/main/proto/google/events/cloud/firestore/v1/data.proto";

/// An event for the trigger `key` of `project`.
pub(crate) struct Event {
    pub project: String,
    pub key: String,
    pub payload: Payload,
}

pub(crate) enum Payload {
    /// The JSON body.
    V1(String),
    V2 {
        event_type: String,
        with_auth: bool,
        /// The database ID.
        database: String,
        /// The document's path, below `documents/`.
        document: String,
        /// The `DocumentEventData`, in base64; shared by the triggers of a change.
        data: Arc<str>,
    },
}

impl Event {
    fn v2(&self) -> bool {
        matches!(self.payload, Payload::V2 { .. })
    }

    /// The trigger's name, as the official emulator's warnings give it.
    fn trigger(&self) -> String {
        let kind = if self.v2() {
            "eventarcTriggers"
        } else {
            "triggers"
        };
        format!("projects/{}/{kind}/{}", self.project, self.key)
    }

    fn path(&self) -> String {
        format!("/functions/projects/{}/triggers/{}", self.project, self.key)
    }
}

/// What the worker shares with the triggers.
struct Shared {
    host: RwLock<Option<String>>,
    log: RwLock<Log>,
    /// Events sent but not answered yet.
    pending: AtomicUsize,
    idle: Notify,
}

impl Shared {
    fn warn(&self, record: &str) {
        let log = Arc::clone(&self.log.read().unwrap_or_else(|e| e.into_inner()));
        log(record);
    }

    fn done(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.idle.notify_waiters();
        }
    }
}

/// Where the events of the triggers go.
pub(crate) struct Delivery {
    shared: Arc<Shared>,
    /// The worker's queue, started with the first event. Dropping it ends the worker.
    queue: OnceLock<Option<mpsc::UnboundedSender<Event>>>,
}

impl Default for Delivery {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                host: RwLock::new(None),
                log: RwLock::new(Arc::new(|record: &str| eprintln!("WARNING: {record}"))),
                pending: AtomicUsize::new(0),
                idle: Notify::new(),
            }),
            queue: OnceLock::new(),
        }
    }
}

impl Delivery {
    pub(crate) fn set_host(&self, host: Option<String>) {
        *self.shared.host.write().unwrap_or_else(|e| e.into_inner()) = host;
    }

    pub(crate) fn set_log(&self, log: Log) {
        *self.shared.log.write().unwrap_or_else(|e| e.into_inner()) = log;
    }

    pub(crate) async fn idle(&self) {
        loop {
            let idle = self.shared.idle.notified();
            if self.shared.pending.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }

    /// Queues `events`, or, without a Functions emulator, says each would have gone.
    pub(crate) fn send(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        let configured = self
            .shared
            .host
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if !configured {
            for event in &events {
                let version = if event.v2() { "v2" } else { "v1" };
                self.shared.warn(&format!(
                    "Failed to publish {version} event for trigger {} because Functions Emulator \
                     host/port is not configured.",
                    event.trigger()
                ));
            }
            return;
        }
        let queue = self.queue.get_or_init(|| {
            let handle = tokio::runtime::Handle::try_current().ok()?;
            let (sender, receiver) = mpsc::unbounded_channel();
            handle.spawn(work(Arc::clone(&self.shared), receiver));
            Some(sender)
        });
        for event in events {
            let Some(queue) = queue else {
                self.shared.warn(&format!(
                    "Failed to publish event for trigger {}: no runtime to send it from",
                    event.trigger()
                ));
                continue;
            };
            if self.shared.pending.load(Ordering::Acquire) >= MAX_PENDING {
                self.shared.warn(&format!(
                    "Dropped an event for trigger {}: {MAX_PENDING} events are waiting for the \
                     Functions emulator",
                    event.trigger()
                ));
                continue;
            }
            self.shared.pending.fetch_add(1, Ordering::AcqRel);
            if queue.send(event).is_err() {
                self.shared.done();
            }
        }
    }
}

/// Sends the queued events, one at a time.
async fn work(shared: Arc<Shared>, mut queue: mpsc::UnboundedReceiver<Event>) {
    while let Some(event) = queue.recv().await {
        let host = shared
            .host
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(host) = host
            && let Some(record) = deliver(&host, &event).await
        {
            shared.warn(&record);
        }
        shared.done();
    }
}

/// Sends `event` to the Functions emulator at `host`; the `WARNING` to give if it failed.
async fn deliver(host: &str, event: &Event) -> Option<String> {
    let path = event.path();
    let (headers, body) = request(event);
    let version = if event.v2() { "v2 " } else { "" };
    match post(host, &path, headers, body).await {
        Ok((status, _, _)) if status.is_success() => None,
        Ok((status, reason, answer)) => {
            let mut record = format!(
                "Unsuccessful HTTP response when publishing {version}event for {}: {} {reason}\n\
                 POST http://{host}{path}",
                event.trigger(),
                status.as_u16()
            );
            if !answer.is_empty() {
                record.push('\n');
                record.push_str(&answer);
            }
            Some(record)
        }
        Err(reason) => Some(format!(
            "Failed to publish {version}event to {host}: {reason}"
        )),
    }
}

/// The headers and the body of `event`'s request.
fn request(event: &Event) -> (Vec<(HeaderName, HeaderValue)>, Vec<u8>) {
    match &event.payload {
        Payload::V1(json) => (
            vec![(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            json.clone().into_bytes(),
        ),
        Payload::V2 {
            event_type,
            with_auth,
            database,
            document,
            data,
        } => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            let mut headers = vec![
                ("ce-specversion", "1.0".to_owned()),
                ("ce-type", event_type.clone()),
                (
                    "ce-source",
                    format!(
                        "//firestore.googleapis.com/projects/{}/databases/{database}",
                        event.project
                    ),
                ),
                ("ce-id", uuid()),
                ("ce-subject", format!("documents/{document}")),
                ("ce-time", rest::timestamp(now, 0)),
                ("ce-datacontenttype", "application/protobuf".to_owned()),
                ("ce-dataschema", DATA_SCHEMA.to_owned()),
                ("ce-location", "us-central1".to_owned()),
                ("ce-project", event.project.clone()),
                ("ce-database", database.clone()),
                ("ce-namespace", "(default)".to_owned()),
                ("ce-document", document.clone()),
            ];
            if *with_auth {
                headers.push(("ce-authtype", "unknown".to_owned()));
                headers.push(("ce-authid", "fake-auth-id@gmail.com".to_owned()));
            }
            headers.push(("content-type", "application/protobuf".to_owned()));
            let headers = headers
                .into_iter()
                .map(|(name, value)| (HeaderName::from_static(name), header_value(&value)))
                .collect();
            (headers, data.as_bytes().to_vec())
        }
    }
}

/// `value` as a header value; the characters a header cannot carry (control characters, as a
/// document ID may hold) become `?`.
fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_bytes(value.as_bytes()).unwrap_or_else(|_| {
        let cleaned: String = value
            .chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        HeaderValue::from_bytes(cleaned.as_bytes()).unwrap_or(HeaderValue::from_static("?"))
    })
}

/// `POST path` to `host` with `headers` and `body`, over a connection of its own: the status,
/// its reason phrase and the body of the answer, or why there is none.
async fn post(
    host: &str,
    path: &str,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
) -> Result<(StatusCode, String, String), String> {
    let stream = match timeout(TIMEOUT, TcpStream::connect(host)).await {
        Err(_) => return Err("Connect timed out".to_owned()),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Err("Connection refused".to_owned());
        }
        Ok(Err(err)) => return Err(err.to_string()),
        Ok(Ok(stream)) => stream,
    };
    let _ = stream.set_nodelay(true);
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|err| err.to_string())?;
    let connection = tokio::spawn(connection);
    let mut request = hyper::Request::post(path)
        .header(header::HOST, header_value(host))
        .header(
            header::USER_AGENT,
            concat!("hidane/", env!("CARGO_PKG_VERSION")),
        );
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let request = request
        .body(Body::from(body))
        .map_err(|err| err.to_string())?;
    let answer = timeout(TIMEOUT, async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status();
        let reason = response
            .extensions()
            .get::<hyper::ext::ReasonPhrase>()
            .map(|reason| String::from_utf8_lossy(reason.as_bytes()).into_owned())
            .or_else(|| status.canonical_reason().map(str::to_owned))
            .unwrap_or_default();
        let body = axum::body::to_bytes(Body::new(response.into_body()), MAX_ANSWER)
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        Ok((status, reason, body))
    })
    .await
    .unwrap_or_else(|_| Err("Read timed out".to_owned()));
    connection.abort();
    answer
}
