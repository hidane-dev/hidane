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

mod firestore;

use std::{io, time::Duration};

use axum::{Router, body::Body, routing::get};
use hidane_proto::google::firestore::v1::firestore_server::FirestoreServer;
use hyper::{Request, body::Incoming, header::CONTENT_TYPE};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
    service::TowerToHyperService,
};
use tokio::net::TcpListener;
use tower::ServiceExt;

pub use firestore::FirestoreService;

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

/// Accepts connections forever, serving gRPC and `http` on the same listener.
pub async fn serve(listener: TcpListener, http: Router) -> io::Result<()> {
    let grpc = grpc_routes();
    // HTTP/2 tuning (keepalive, max message size) is tracked separately; defaults for now.
    let builder = auto::Builder::new(TokioExecutor::new());
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(err) => {
                // Transient (e.g. too many open files): back off instead of exiting.
                eprintln!("accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let (grpc, http, builder) = (grpc.clone(), http.clone(), builder.clone());
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
            // Connection-level errors (client reset, malformed preface) only affect this client.
            let _ = builder
                .serve_connection_with_upgrades(
                    TokioIo::new(stream),
                    TowerToHyperService::new(service),
                )
                .await;
        });
    }
}
