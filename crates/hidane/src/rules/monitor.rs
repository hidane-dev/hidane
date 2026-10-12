//! The records of the Emulator UI's request monitor (its Firestore "Requests" tab): one per
//! evaluation of Security Rules, sent over the WebSocket feed on `--websocket_port`
//! (`crate::websocket`), as the official emulator sends them
//! (`tests/fixtures/websocket_requests.json`, `tools/oracle/websocket_requests.py`).
//!
//! A record is a JSON object, written without spaces:
//!
//! ```text
//! {"rules":…,"time":…,"rulesReleaseKey":…,"requestId":…,"outcome":…,
//!  "granularAllowOutcomes":[{"line":5,"outcome":true},…],
//!  "rulesContext":{"method":…,"path":…,"time":…,"request":…,"resource":…}}
//! ```
//!
//! - `rules` is the ruleset's (first) file; a database without rules shows the open rules the
//!   official emulator applies then, and its requests are recorded too. Requests of the owner
//!   (`Bearer owner`) are not checked and not recorded.
//! - `time` is when the record was made, `rulesContext.time` the request's `request.time`.
//! - `rulesReleaseKey` is `projects/{p}/releases/cloud.firestore`, then `/{database}` but for
//!   `(default)`, then a number: the official emulator's changes with the ruleset and from run
//!   to run; hidane's with the ruleset. `requestId` is a random UUID. The Emulator UI keeps the
//!   records of every project: it filters on `rulesReleaseName`, which neither emulator sends.
//! - `outcome` is `allow` if a statement allowed, else `error` if a condition failed, else
//!   `deny`; `granularAllowOutcomes` lists each statement evaluated, by the line of its
//!   condition, and whether it allowed. That list grows over the evaluations of one request:
//!   each write of a batch, each document of a batchGet or a Listen target, each way of a list.
//! - `rulesContext.path` is the document's path from `/databases`; a list's is its collection's
//!   followed by `/*`, and for a collection group `/**/{id}/*` below its parent; a create with a
//!   document ID the server chooses, its collection's followed by `/*`.
//! - `request` and `resource` are value messages as `GET :ruleCoverage` and `debug()` show them
//!   (`super::report`), their fields in the official emulator's order, as the evaluator holds
//!   them.
//!
//! The official emulator also evaluates a request without its stored document first, and each
//! such pass is a record too; hidane evaluates once (`docs/parity-exceptions.md`).

use std::{
    collections::VecDeque,
    fmt::Write as _,
    hash::BuildHasher as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_rules::eval::{Evaluation, Method, Observed, Request, Verdict};
use tokio::sync::{Notify, mpsc};

use super::{File, json_string, report};

/// How many records a client gets when it connects: the last ones, oldest first, as on the
/// official emulator. Neither `PUT :securityRules` nor `POST /reset` clears them.
pub const BACKLOG: usize = 50;

/// How many bytes of records may wait for one client. A client further behind is disconnected
/// (the Emulator UI reconnects, and gets the backlog); the official emulator waits for it
/// instead, holding up the requests being checked (`docs/parity-exceptions.md`).
pub const CLIENT_BUFFER: usize = 16 * 1024 * 1024;

/// The records and the clients of the feed.
pub struct Monitor {
    state: Mutex<State>,
    ids: RequestIds,
}

#[derive(Default)]
struct State {
    backlog: VecDeque<Arc<str>>,
    clients: Vec<Client>,
}

struct Client {
    records: mpsc::UnboundedSender<Arc<str>>,
    queued: Arc<AtomicUsize>,
    dropped: Arc<Notify>,
}

/// A client of the feed: the backlog when it connected (a JSON array), then each record.
pub(crate) struct Subscription {
    pub backlog: String,
    pub records: mpsc::UnboundedReceiver<Arc<str>>,
    /// The bytes of the records not yet sent; the receiver subtracts what it sends.
    pub queued: Arc<AtomicUsize>,
    /// Notified when the client fell more than [`CLIENT_BUFFER`] behind.
    pub dropped: Arc<Notify>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            ids: RequestIds::new(),
        }
    }
}

impl Monitor {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new client: the backlog and every record from now on, none missed or repeated.
    pub(crate) fn subscribe(&self) -> Subscription {
        let (sender, records) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(Notify::new());
        let mut state = self.state();
        let mut backlog = String::from("[");
        for (i, record) in state.backlog.iter().enumerate() {
            if i > 0 {
                backlog.push(',');
            }
            backlog.push_str(record);
        }
        backlog.push(']');
        state.clients.push(Client {
            records: sender,
            queued: Arc::clone(&queued),
            dropped: Arc::clone(&dropped),
        });
        Subscription {
            backlog,
            records,
            queued,
            dropped,
        }
    }

    /// Keeps `record` in the backlog and queues it for every client, never waiting for one.
    fn publish(&self, record: String) {
        let record: Arc<str> = record.into();
        let mut state = self.state();
        if state.backlog.len() == BACKLOG {
            state.backlog.pop_front();
        }
        state.backlog.push_back(Arc::clone(&record));
        state.clients.retain(|client| {
            let queued = client.queued.load(Ordering::Acquire);
            if queued > 0 && queued + record.len() > CLIENT_BUFFER {
                client.dropped.notify_one();
                return false;
            }
            client.queued.fetch_add(record.len(), Ordering::AcqRel);
            client.records.send(Arc::clone(&record)).is_ok()
        });
    }

    /// The records of one request checked against `files` in `database`
    /// (`projects/{p}/databases/{d}`).
    pub(crate) fn session(self: &Arc<Self>, database: &str, files: &[File]) -> Session {
        let mut parts = database.split('/');
        let project = parts.nth(1).unwrap_or_default();
        let database_id = parts.nth(1).unwrap_or_default();
        let mut release = format!("projects/{project}/releases/cloud.firestore");
        if database_id != "(default)" {
            let _ = write!(release, "/{database_id}");
        }
        let _ = write!(release, "{}", fingerprint(files));
        Session {
            monitor: Arc::clone(self),
            rules: json_string(files.first().map_or("", |f| f.content.as_str())),
            release: json_string(&release),
            granular: Mutex::default(),
        }
    }
}

/// A number for a ruleset: Java's `String.hashCode` over its files.
fn fingerprint(files: &[File]) -> i32 {
    let mut hash = 0i32;
    for file in files {
        let name = file.name.as_deref().unwrap_or_default();
        for unit in name.encode_utf16().chain(file.content.encode_utf16()) {
            hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
        }
        for &byte in &file.fingerprint {
            hash = hash.wrapping_mul(31).wrapping_add(i32::from(byte));
        }
    }
    hash
}

/// The records of one request: what its evaluations share.
pub(crate) struct Session {
    monitor: Arc<Monitor>,
    /// The ruleset's text and release key, as JSON strings.
    rules: String,
    release: String,
    /// `granularAllowOutcomes` so far, without the brackets.
    granular: Mutex<String>,
}

impl Session {
    /// Records each evaluation of `request` the trace reported.
    pub(crate) fn report(&self, request: &Request, evaluations: &[Evaluation]) {
        for evaluation in evaluations {
            let record = self.record(request, evaluation);
            self.monitor.publish(record);
        }
    }

    fn record(&self, request: &Request, evaluation: &Evaluation) -> String {
        let verdicts = || evaluation.statements.iter().map(|(_, v)| *v);
        let outcome = if verdicts().any(|v| v == Verdict::Allow) {
            "allow"
        } else if verdicts().any(|v| v == Verdict::Error) {
            "error"
        } else {
            "deny"
        };
        let granular = {
            let mut granular = self.granular.lock().unwrap_or_else(|e| e.into_inner());
            for (line, verdict) in &evaluation.statements {
                if !granular.is_empty() {
                    granular.push(',');
                }
                let _ = write!(
                    granular,
                    "{{\"line\":{line},\"outcome\":{}}}",
                    *verdict == Verdict::Allow
                );
            }
            granular.clone()
        };
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let now = crate::rest::timestamp(
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            i32::try_from(since.subsec_micros() * 1000).unwrap_or(0),
        );
        let time = crate::rest::timestamp(request.time.seconds, request.time.nanos);
        let mut out = String::with_capacity(self.rules.len() + 1024);
        let _ = write!(
            out,
            "{{\"rules\":{},\"time\":\"{now}\",\"rulesReleaseKey\":{},\"requestId\":\"{}\",\
             \"outcome\":\"{outcome}\",\"granularAllowOutcomes\":[{granular}],\"rulesContext\":\
             {{\"method\":\"{}\",\"path\":{},\"time\":\"{time}\",\"request\":",
            self.rules,
            self.release,
            self.monitor.ids.next(),
            evaluation.method.name(),
            json_string(&context_path(request, evaluation)),
        );
        report::value(&evaluation.request).compact(&mut out);
        out.push_str(",\"resource\":");
        report::value(&evaluation.resource).compact(&mut out);
        out.push_str("}}");
        out
    }
}

/// `rulesContext.path`: the document's path from `/databases`, or for a list its collection's
/// and `/*` (for a collection group, `/**/{id}/*` below its parent). A list decided on one
/// document (`__name__ ==`) shows that document; a create with an automatic ID, its collection's
/// and `/*`.
fn context_path(request: &Request, evaluation: &Evaluation) -> String {
    let mut path = String::new();
    if evaluation.method == Method::List
        && let Some(Observed::Path(segments)) = field(&evaluation.request, "path")
        && segments.len() % 2 == 1
    {
        for segment in segments {
            if let hidane_rules::eval::Segment::Simple(s) = segment {
                let _ = write!(path, "/{s}");
            }
        }
        return path;
    }
    for segment in &request.path {
        let _ = write!(path, "/{segment}");
    }
    if evaluation.method == Method::List {
        if let Some(group) = request.query.as_ref().and_then(|q| q.group.as_deref()) {
            path.push_str("/**");
            if !group.is_empty() {
                let _ = write!(path, "/{group}");
            }
        }
        path.push_str("/*");
    } else if request.auto_id {
        path.push_str("/*");
    }
    path
}

fn field<'a>(value: &'a Observed, name: &str) -> Option<&'a Observed> {
    match value {
        Observed::Map(fields) => fields.iter().find(|(k, _)| k == name).map(|(_, v)| v),
        _ => None,
    }
}

/// Random version 4 UUIDs, without a random number generator: two random keys from the
/// standard library's hasher seeds, mixed with a counter.
struct RequestIds {
    keys: (u64, u64),
    count: AtomicU64,
}

impl RequestIds {
    fn new() -> Self {
        let random = || std::collections::hash_map::RandomState::new().hash_one(0u8);
        Self {
            keys: (random(), random()),
            count: AtomicU64::new(0),
        }
    }

    fn next(&self) -> String {
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        let high = mix(self.keys.0 ^ n);
        let low = mix(self.keys.1.wrapping_add(n));
        let high = (high & !0xf000) | 0x4000;
        let low = (low & !(0b11 << 62)) | (0b10 << 62);
        format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            high >> 32,
            (high >> 16) & 0xffff,
            high & 0xffff,
            low >> 48,
            low & 0xffff_ffff_ffff
        )
    }
}

/// SplitMix64's finalizer: a bijection that scatters the bits.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_version_4_uuids() {
        let ids = RequestIds::new();
        let (a, b) = (ids.next(), ids.next());
        assert_ne!(a, b);
        for id in [a, b] {
            let parts: Vec<&str> = id.split('-').collect();
            assert_eq!(
                parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
                [8, 4, 4, 4, 12]
            );
            assert!(parts[2].starts_with('4'), "{id}");
            assert!(matches!(&parts[3][..1], "8" | "9" | "a" | "b"), "{id}");
        }
    }

    #[test]
    fn the_backlog_keeps_the_last_records() {
        let monitor = Monitor::new();
        for i in 0..BACKLOG + 5 {
            monitor.publish(i.to_string());
        }
        let backlog = monitor.subscribe().backlog;
        let expected: Vec<String> = (5..BACKLOG + 5).map(|i| i.to_string()).collect();
        assert_eq!(backlog, format!("[{}]", expected.join(",")));
    }

    #[test]
    fn a_client_too_far_behind_is_dropped() {
        let monitor = Monitor::new();
        let mut subscription = monitor.subscribe();
        let big = "x".repeat(CLIENT_BUFFER / 2 + 1);
        monitor.publish(big.clone());
        assert_eq!(monitor.state().clients.len(), 1);
        monitor.publish(big.clone());
        assert!(monitor.state().clients.is_empty());
        assert_eq!(subscription.records.try_recv().unwrap().len(), big.len());
        // A record larger than the buffer still reaches a client that is not behind.
        let mut other = monitor.subscribe();
        monitor.publish("y".repeat(CLIENT_BUFFER + 1));
        assert!(other.records.try_recv().is_ok());
    }
}
