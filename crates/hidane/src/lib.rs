//! hidane (火種, *the seed of fire*) — a Firestore emulator without Java.
//!
//! # One port, three protocols
//!
//! Like the official emulator, hidane accepts every protocol on a single TCP port:
//!
//! - gRPC over plaintext HTTP/2 (h2c prior knowledge), used by the server and mobile SDKs;
//! - REST over HTTP/1.1, used by `curl`, the Emulator UI, firebase-tools and js-sdk Lite;
//! - WebChannel over HTTP/1.1, used by firebase-js-sdk in the browser.
//!
//! `hyper_util::server::conn::auto` detects the HTTP/2 connection preface per connection, so
//! HTTP/1.1 and h2c share the listener. Each request is then dispatched by **content type**:
//! `application/grpc*` goes to the tonic services, everything else to the HTTP router.
//! Dispatching by path would not work: WebChannel URLs such as
//! `/google.firestore.v1.Firestore/Listen/channel` sit under the gRPC service prefix.

pub mod cli;
mod datastore;
pub mod diagnostic;
pub mod export;
mod firestore;
mod gson;
mod json_format;
pub mod migrate;
mod ordered_json;
pub mod project;
mod rest;
pub mod rules;
pub mod single_project;
pub mod triggers;
mod webchannel;
mod websocket;

use std::{
    future::Future,
    io,
    net::{Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use hidane_core::{
    export::ExportedDocument,
    store::{MemoryStore, Store},
};
use hidane_proto::google::firestore::v1::firestore_server::FirestoreServer;
use hyper::{Request, body::Incoming, header::CONTENT_TYPE};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::{conn::auto, graceful::GracefulShutdown},
    service::TowerToHyperService,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};
use tower::ServiceExt;

pub use firestore::FirestoreService;
use firestore::{
    changes::ChangeFeed,
    seed::{SeededStore, Seeder},
    transactions::Transactions,
};
pub use websocket::serve_websocket;

/// How long open connections (e.g. Listen streams) get to finish after a shutdown signal.
/// firebase-tools waits 4 s after SIGINT before giving up on the process.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The emulator's state shared by every protocol: the store, the open transactions, the
/// Security Rules, the Functions emulator's triggers and the shutdown flag.
///
/// The shutdown request is a sticky flag rather than a one-shot notification, so every waiter
/// sees it, including one that starts waiting after the request arrived.
#[derive(Clone)]
pub struct Admin {
    store: Arc<dyn Store>,
    transactions: Arc<Transactions>,
    changes: Arc<ChangeFeed>,
    shutdown: Arc<watch::Sender<bool>>,
    /// `--database-mode datastore-mode`: the Datastore API in place of Firestore's (#81).
    datastore: Option<datastore::DatastoreService>,
    enterprise: bool,
    /// `--require_indexes` in Firestore Native mode: queries are refused.
    refuse_queries: bool,
    channels: Arc<webchannel::Channels>,
    seeder: Option<Arc<Seeder>>,
    rules: Arc<rules::Rules>,
    triggers: Arc<triggers::Triggers>,
    single_project: Option<Arc<single_project::SingleProject>>,
    index_usage: Arc<firestore::index_usage::IndexUsage>,
}

impl Default for Admin {
    /// A fresh in-memory store.
    fn default() -> Self {
        Self::new(Arc::new(MemoryStore::new()))
    }
}

impl Admin {
    fn firestore(&self) -> FirestoreService {
        FirestoreService::with_state(
            Arc::clone(&self.store),
            Arc::clone(&self.transactions),
            Arc::clone(&self.changes),
        )
        .with_enterprise_edition(self.enterprise)
        .with_seeder(self.seeder.clone())
        .with_rules(Arc::clone(&self.rules))
        .with_triggers(Arc::clone(&self.triggers))
        .with_single_project(self.single_project.clone())
        .with_index_usage(Arc::clone(&self.index_usage))
        .with_refused_queries(self.refuse_queries)
        .with_datastore_mode(self.datastore.is_some())
    }

    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            transactions: Arc::default(),
            changes: Arc::default(),
            shutdown: Arc::new(watch::Sender::new(false)),
            datastore: None,
            enterprise: false,
            refuse_queries: false,
            channels: Arc::default(),
            seeder: None,
            rules: Arc::default(),
            triggers: Arc::default(),
            single_project: None,
            index_usage: Arc::default(),
        }
    }

    /// The Security Rules, per database.
    pub fn rules(&self) -> &rules::Rules {
        &self.rules
    }

    /// The Firestore triggers the Functions emulator registered, and where their events go
    /// (`--functions_emulator`).
    pub fn triggers(&self) -> &triggers::Triggers {
        &self.triggers
    }

    /// `--seed_from_export`: every database starts with the documents of `documents` whose
    /// key names it, handed over on its first access and again after `POST /reset`.
    #[must_use]
    pub fn with_seed(mut self, documents: Vec<ExportedDocument>) -> Self {
        let seeder = Arc::new(Seeder::new(documents, Arc::clone(&self.changes)));
        self.store = Arc::new(SeededStore {
            inner: self.store,
            seeder: Arc::clone(&seeder),
        });
        self.seeder = Some(seeder);
        self
    }

    /// `--project_id project --single_project_mode`, and `--single_project_mode_error` if
    /// `error`: requests for another project print a warning, or fail.
    #[must_use]
    pub fn with_single_project(mut self, project: impl Into<String>, error: bool) -> Self {
        self.single_project = Some(Arc::new(single_project::SingleProject::new(project, error)));
        self
    }

    /// `--single_project_mode`, if given.
    pub fn single_project(&self) -> Option<&single_project::SingleProject> {
        self.single_project.as_deref()
    }

    /// `--database-mode datastore-mode`: the Datastore API is served, and the Firestore API
    /// refused, as the official emulator does in that mode (#81).
    #[must_use]
    pub fn with_datastore_mode(mut self, datastore: bool) -> Self {
        self.datastore = datastore.then(datastore::DatastoreService::default);
        self
    }

    /// `--import-data` in Datastore mode: the entities of an export, each in the project and
    /// database its key names. After [`Admin::with_datastore_mode`]; nothing in Firestore Native
    /// mode.
    #[must_use]
    pub fn with_datastore_seed(
        self,
        entities: Vec<(
            hidane_proto::google::datastore::v1::Key,
            hidane_proto::ordered::Properties,
        )>,
    ) -> Self {
        if let Some(datastore) = &self.datastore {
            datastore.seed(entities);
        }
        self
    }

    /// `--require_indexes` in Datastore mode: queries need the indexes `--index_file` defines
    /// (`file`), read as the official emulator reads them (#81). After
    /// [`Admin::with_datastore_mode`]; nothing in Firestore Native mode.
    #[must_use]
    pub fn with_datastore_indexes(mut self, file: Option<std::path::PathBuf>) -> Self {
        if self.datastore.is_some() {
            self.datastore = Some(datastore::DatastoreService::with_indexes(file));
        }
        self
    }

    /// `--require_indexes` in Firestore Native mode: the emulator starts, and every query is
    /// refused ("Firestore Native mode does not support index enforcement"), as the official
    /// emulator does; other requests are served.
    #[must_use]
    pub fn with_firestore_index_enforcement(mut self, refuse_queries: bool) -> Self {
        self.refuse_queries = refuse_queries;
        self
    }

    /// `--database-edition enterprise`.
    #[must_use]
    pub fn with_enterprise_edition(mut self, enterprise: bool) -> Self {
        self.enterprise = enterprise;
        self
    }

    pub fn store(&self) -> Arc<dyn Store> {
        Arc::clone(&self.store)
    }

    /// Resolves once `POST /shutdown` has been received.
    pub async fn shutdown_requested(&self) {
        let mut requested = self.shutdown.subscribe();
        // The sender lives as long as `self`, so `wait_for` cannot fail while we wait.
        let _ = requested.wait_for(|requested| *requested).await;
    }

    fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

/// Plain HTTP routes (REST, admin endpoints, later WebChannel), all answering CORS as the
/// official emulator does (routes a caller adds to the returned router do not).
///
/// The admin endpoints match the official emulator exactly: `GET /` answers `Ok`, `POST /reset`
/// and `POST /shutdown` act, the query string is ignored, and any other method, a trailing
/// slash, another case or another prefix gets `404 Not Found`. Bodies end with a newline, as
/// the official ones do.
pub fn http_routes(admin: Admin) -> Router {
    let routes = Router::new()
        // axum answers HEAD from the GET handler by default; the official emulator does not.
        .route(
            "/",
            get(|| async { text("Ok\n") })
                .head(not_found)
                .fallback(not_found),
        )
        .route("/reset", post(reset).fallback(not_found))
        .route("/shutdown", post(shutdown).fallback(not_found))
        .route(
            "/emulator/v1/projects/{project}/databases/{database}/documents",
            delete(clear_database).fallback(not_found),
        )
        .route(
            "/emulator/v1/projects/{project}/databases/{database}/documents/",
            delete(clear_database).fallback(not_found),
        )
        .route(
            "/emulator/v1/projects/{project}/databases/{database}/documents/{*path}",
            delete(delete_tree).fallback(not_found),
        )
        // WebChannel, the browser SDK's transport for the Listen and Write streams.
        .route(
            "/google.firestore.v1.Firestore/{rpc}/channel",
            get(channel_get).post(channel_post).fallback(not_found),
        )
        .fallback(fallback)
        .with_state(admin);
    // Around the whole router rather than each route, so a method router's `Allow` header
    // never reaches a preflight.
    Router::new()
        .fallback_service(routes)
        .layer(axum::middleware::from_fn(cors))
}

async fn not_found() -> Response {
    not_found_response()
}

/// CORS as the official emulator answers it, on every HTTP path: with an `Origin`, any
/// response reflects it and allows credentials and every method; an `OPTIONS` request is a
/// preflight whatever the path, answered `200` with an empty body, allowing the requested
/// headers and, when asked, the private network. Without an `Origin`, no CORS headers.
async fn cors(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::http::{HeaderValue, header};
    const METHODS: HeaderValue = HeaderValue::from_static("DELETE,GET,HEAD,PATCH,POST,PUT");
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = if request.method() == axum::http::Method::OPTIONS {
        let mut response = Response::new(Body::empty());
        if origin.is_some() {
            let headers = response.headers_mut();
            if let Some(requested) = request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            {
                headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
            }
            // Whatever its value, as on the official emulator.
            if request
                .headers()
                .contains_key("access-control-request-private-network")
            {
                headers.insert(
                    "access-control-allow-private-network",
                    HeaderValue::from_static("true"),
                );
            }
        }
        response
    } else {
        next.run(request).await
    };
    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, METHODS);
    }
    response
}

async fn channel_get(
    State(admin): State<Admin>,
    Path(rpc): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    match webchannel::Kind::from_rpc(&rpc) {
        Some(kind) => admin
            .channels
            .get_back_channel(kind, query.as_deref().unwrap_or_default()),
        None => not_found_response(),
    }
}

async fn channel_post(
    State(admin): State<Admin>,
    Path(rpc): Path<String>,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    let Some(kind) = webchannel::Kind::from_rpc(&rpc) else {
        return not_found_response();
    };
    let Ok(body) = axum::body::to_bytes(body, MAX_CHANNEL_BODY).await else {
        return not_found_response();
    };
    admin
        .channels
        .post(
            admin.firestore(),
            kind,
            query.as_deref().unwrap_or_default(),
            &body,
        )
        .await
}

/// The largest WebChannel POST read (as REST bodies).
const MAX_CHANNEL_BODY: usize = 16 * 1024 * 1024;

/// Security Rules (`/emulator/v1/projects/{p}:securityRules`, `:ruleCoverage`), export and
/// import (`/emulator/v1/projects/{p}:export`, `:import`), the Functions emulator's trigger
/// registrations (`/emulator/v1/projects/{p}/triggers/{key}`, `…/eventarcTrigger`), the
/// indexes queries needed (`/emulator/v1/projects/{p}:indexUsage`), REST (`/v1/…`,
/// `/v1beta1/…`), or 404.
async fn fallback(State(admin): State<Admin>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    if let Some(service) = &admin.datastore {
        if let Some((project, verb)) = datastore::rest::route(parts.uri.path()) {
            return datastore::rest::handle(
                service,
                &parts.method,
                &parts.headers,
                &project,
                &verb,
                body,
            )
            .await;
        }
        // The emulator's own endpoints fail in this mode, as on the official emulator; the
        // Firestore API is routed and refused by the Firestore service.
        if is_index_usage(parts.uri.path())
            || triggers::route(parts.uri.path()).is_some()
            || rules::route(parts.uri.path())
                .is_some_and(|(_, _, verb)| verb != rules::Verb::RuleCoverageHtml)
        {
            return rest::error(&tonic::Status::unknown(""));
        }
    }
    if is_index_usage(parts.uri.path()) {
        return index_usage(&admin, &parts.method, parts.uri.query());
    }
    if let Some(route) = rules::route(parts.uri.path()) {
        return rules::handle(
            &admin.rules,
            admin.single_project(),
            route,
            &parts.method,
            parts.uri.query(),
            &parts.headers,
            body,
        )
        .await;
    }
    if let Some(verb) = export::route(parts.uri.path()) {
        return export::handle(
            admin.firestore(),
            admin.datastore.as_ref(),
            verb,
            &parts.method,
            &parts.headers,
            body,
        )
        .await;
    }
    if let Some(route) = triggers::route(parts.uri.path()) {
        return triggers::handle(
            &admin.triggers,
            route,
            &parts.method,
            parts.uri.query(),
            &parts.headers,
            body,
        )
        .await;
    }
    rest::handle(
        admin.firestore(),
        parts.method,
        parts.uri.path(),
        parts.uri.query(),
        &parts.headers,
        body,
    )
    .await
    .unwrap_or_else(not_found_response)
}

/// `/emulator/v1/projects/{p}:indexUsage`; not under a database.
fn is_index_usage(path: &str) -> bool {
    path.strip_prefix("/emulator/v1/projects/")
        .and_then(|rest| rest.strip_suffix(":indexUsage"))
        .is_some_and(|project| !project.contains('/'))
}

/// `GET /emulator/v1/projects/{p}:indexUsage?database=projects/{p}/databases/{d}`: the
/// composite indexes the queries run on that database needed. The project of the path is not
/// read (`tests/fixtures/index_usage.json`).
fn index_usage(admin: &Admin, method: &axum::http::Method, query: Option<&str>) -> Response {
    if method != axum::http::Method::GET {
        return not_found_response();
    }
    let database = rest::parse_query(query.unwrap_or_default())
        .into_iter()
        .find_map(|(key, value)| (key == "database").then_some(value))
        .unwrap_or_default();
    match firestore::names::database(&database) {
        Ok(database) => (
            [(header::CONTENT_TYPE, "application/json")],
            admin.index_usage.report(&database),
        )
            .into_response(),
        Err(status) => rest::error(&status),
    }
}

/// The official emulator's 404: plain text without a content type.
fn not_found_response() -> Response {
    let mut response = text("Not Found\n");
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

/// The official emulator's admin bodies are plain text without a content type.
fn text(body: &'static str) -> Response {
    Response::new(Body::from(body))
}

async fn reset(State(admin): State<Admin>) -> Response {
    // Every document of every project, and every open transaction with its locks, as on the
    // official emulator; attached listeners see the documents go, which the official emulator
    // does not tell them. The rulesets loaded since the start go too, with their coverage.
    admin.firestore().reset().await;
    if let Some(datastore) = &admin.datastore {
        datastore.reset();
    }
    admin.rules.reset();
    admin.index_usage.clear();
    text("Resetting...\n")
}

/// `DELETE /emulator/v1/projects/{p}/databases/{d}/documents`, what `clearFirestore()` of
/// rules-unit-testing and the Emulator UI's "Clear all data" call. Any caller may clear, as on
/// the official emulator, but an `Authorization` header must still read.
async fn clear_database(
    State(admin): State<Admin>,
    Path((project, database)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if admin.datastore.is_some() {
        // In Datastore mode, as on the official emulator.
        return rest::error(&tonic::Status::unknown(""));
    }
    // The name is read first, as a parent's (`tools/oracle/project_ids.py`).
    let database = format!("projects/{}/databases/{}", project.trim(), database.trim());
    let firestore = admin.firestore();
    if let Err(status) = firestore::names::parent(&format!("{database}/documents")).and_then(|_| {
        firestore.admit(&database, || {
            firestore::auth::from_header(authorization(&headers))
        })
    }) {
        return rest::error(&status);
    }
    firestore.clear_database(&database).await;
    empty_json()
}

/// `DELETE /emulator/v1/projects/{p}/databases/{d}/documents/{path}`: the Emulator UI's
/// recursive delete of a document or collection. Whitespace around each segment is trimmed, as
/// in REST paths.
async fn delete_tree(
    State(admin): State<Admin>,
    Path((project, database, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if admin.datastore.is_some() {
        // In Datastore mode, as clearing a whole database there.
        return rest::error(&tonic::Status::unknown(""));
    }
    let database = format!("projects/{}/databases/{}", project.trim(), database.trim());
    let path = path.split('/').map(str::trim).collect::<Vec<_>>().join("/");
    match admin
        .firestore()
        .delete_tree(&database, &path, authorization(&headers))
        .await
    {
        Ok(()) => empty_json(),
        Err(status) => rest::error(&status),
    }
}

fn authorization(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
}

/// The official emulator's empty success body.
fn empty_json() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], "{\n}\n").into_response()
}

async fn shutdown(State(admin): State<Admin>) -> Response {
    // `serve` stops accepting and drains connections, so this response is still delivered.
    admin.request_shutdown();
    text("Shutting down...\n")
}

/// gRPC services: `google.firestore.v1.Firestore` plus server reflection (v1 and v1alpha, so
/// both current and older `grpcurl` versions can list and describe the API).
/// The largest gRPC message the official emulator accepts (tonic's default is 4 MiB, less than
/// a large batch).
const MAX_GRPC_MESSAGE: usize = 100 * 1024 * 1024;

pub fn grpc_routes(admin: &Admin) -> Router {
    let reflection = || {
        tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(hidane_proto::FILE_DESCRIPTOR_SET)
    };
    let reflection_v1 = reflection()
        .build_v1()
        .expect("embedded descriptor set is valid");
    let reflection_v1alpha = reflection()
        .build_v1alpha()
        .expect("embedded descriptor set is valid");
    // In Datastore mode every Firestore RPC is refused, as on the official emulator (#81).
    // In Datastore mode, each Firestore RPC is refused once it has read its names
    // (`FirestoreService::caller`), as the official emulator refuses them.
    let firestore =
        FirestoreServer::new(admin.firestore()).max_decoding_message_size(MAX_GRPC_MESSAGE);
    let v1beta1 = firestore.clone();
    let mut routes = tonic::service::Routes::new(firestore)
        .add_service(reflection_v1)
        .add_service(reflection_v1alpha);
    if let Some(service) = &admin.datastore {
        routes = routes.add_service(
            hidane_proto::google::datastore::v1::datastore_server::DatastoreServer::new(
                service.clone(),
            )
            .max_decoding_message_size(MAX_GRPC_MESSAGE),
        );
    }
    routes
        .into_axum_router()
        // `google.firestore.v1beta1.Firestore` is the same service under its earlier name, as
        // on the official emulator; its messages are wire-compatible with v1's.
        .route_service(
            "/google.firestore.v1beta1.Firestore/{method}",
            tower::service_fn(move |mut request: axum::extract::Request| {
                let v1beta1 = v1beta1.clone();
                async move {
                    let path = request.uri().path().replacen(
                        "/google.firestore.v1beta1.",
                        "/google.firestore.v1.",
                        1,
                    );
                    *request.uri_mut() = path.parse().expect("a path with a known prefix");
                    request.extensions_mut().insert(firestore::V1beta1);
                    v1beta1.oneshot(request).await
                }
            }),
        )
}

fn is_grpc<B>(req: &Request<B>) -> bool {
    req.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/grpc"))
}

/// Binds the listeners for `--host` / `--port`.
///
/// `localhost` binds 127.0.0.1 and, when available, ::1 on the same port: SDKs resolve
/// `localhost` differently (Node may try ::1 first), and binding only one family makes the
/// other fail to connect. Any other host binds the first address it resolves to.
pub async fn bind(host: &str, port: u16) -> io::Result<Vec<TcpListener>> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        if let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
            listeners.push(v6);
        }
        return Ok(listeners);
    }
    Ok(vec![TcpListener::bind((host, port)).await?])
}

/// Serves `grpc` and `http` on every listener until `shutdown` resolves, then stops accepting
/// and gives open connections [`SHUTDOWN_GRACE`] to finish.
pub async fn serve(
    listeners: Vec<TcpListener>,
    grpc: Router,
    http: Router,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    // hyper's HTTP/2 defaults, on purpose (#25): keepalive pings are answered, where the official
    // emulator sends GOAWAY (docs/parity-exceptions.md). Message sizes are limited per service
    // (MAX_GRPC_MESSAGE, MAX_CHANNEL_BODY).
    let builder = auto::Builder::new(TokioExecutor::new());
    let graceful = GracefulShutdown::new();

    let (accepted_tx, mut accepted) = mpsc::channel::<TcpStream>(64);
    let acceptors: Vec<_> = listeners
        .into_iter()
        .map(|listener| tokio::spawn(accept_loop(listener, accepted_tx.clone())))
        .collect();
    drop(accepted_tx);

    tokio::pin!(shutdown);
    loop {
        let stream = tokio::select! {
            stream = accepted.recv() => match stream {
                Some(stream) => stream,
                None => break,
            },
            () = &mut shutdown => break,
        };
        let (grpc, http, builder) = (grpc.clone(), http.clone(), builder.clone());
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            let service = tower::service_fn(move |req: Request<Incoming>| {
                let target = if is_grpc(&req) {
                    grpc.clone()
                } else {
                    http.clone()
                };
                // `Router` is infallible, so the error type stays `Infallible`.
                target.oneshot(req.map(Body::new))
            });
            let connection = builder.serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerToHyperService::new(service),
            );
            // Connection-level errors (client reset, malformed preface) only affect this client.
            let _ = watcher.watch(connection).await;
        });
    }

    // Stop accepting first so the port is released immediately, then drain.
    for acceptor in acceptors {
        acceptor.abort();
    }
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, graceful.shutdown()).await;
    Ok(())
}

async fn accept_loop(listener: TcpListener, accepted: mpsc::Sender<TcpStream>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                if accepted.send(stream).await.is_err() {
                    return;
                }
            }
            Err(err) => {
                // Transient (e.g. too many open files): back off instead of exiting.
                eprintln!("accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
