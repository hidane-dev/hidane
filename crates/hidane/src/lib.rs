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
mod firestore;

use std::{
    future::Future,
    io,
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use axum::{Router, body::Body, routing::get};
use hidane_proto::google::firestore::v1::firestore_server::FirestoreServer;
use hyper::{Request, body::Incoming, header::CONTENT_TYPE};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::{conn::auto, graceful::GracefulShutdown},
    service::TowerToHyperService,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tower::ServiceExt;

pub use firestore::FirestoreService;

/// How long open connections (e.g. Listen streams) get to finish after a shutdown signal.
/// firebase-tools waits 4 s after SIGINT before giving up on the process.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Plain HTTP routes (REST, admin endpoints, later WebChannel). Callers may add routes before
/// handing the router to [`serve`].
pub fn http_routes() -> Router {
    // The official emulator answers `GET /` with `200 Ok`; firebase-tools and humans use it as
    // a liveness check.
    Router::new().route("/", get(|| async { "Ok" }))
}

/// gRPC services: `google.firestore.v1.Firestore` plus server reflection (v1 and v1alpha, so
/// both current and older `grpcurl` versions can list and describe the API).
pub fn grpc_routes() -> Router {
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
    tonic::service::Routes::new(FirestoreServer::new(FirestoreService))
        .add_service(reflection_v1)
        .add_service(reflection_v1alpha)
        .into_axum_router()
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

/// Serves gRPC and `http` on every listener until `shutdown` resolves, then stops accepting and
/// gives open connections [`SHUTDOWN_GRACE`] to finish.
pub async fn serve(
    listeners: Vec<TcpListener>,
    http: Router,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    let grpc = grpc_routes();
    // HTTP/2 tuning (keepalive, max message size) is tracked in #25; defaults for now.
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
