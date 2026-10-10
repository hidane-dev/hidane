//! One listener serves gRPC (h2c), REST (HTTP/1.1) and long-lived chunked responses (the shape
//! of a WebChannel back channel). See ADR 0003.

use std::{
    convert::Infallible,
    net::SocketAddr,
    time::{Duration, Instant},
};

use axum::{Router, body::Body, routing::get};
use bytes::Bytes;
use hidane_proto::google::firestore::v1::{GetDocumentRequest, firestore_client::FirestoreClient};
use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request, StatusCode};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;

async fn start(http: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let grpc = hidane::grpc_routes(&hidane::Admin::default());
    tokio::spawn(hidane::serve(
        vec![listener],
        grpc,
        http,
        std::future::pending(),
    ));
    addr
}

fn http1_client() -> Client<hyper_util::client::legacy::connect::HttpConnector, Empty<Bytes>> {
    Client::builder(TokioExecutor::new()).build_http()
}

#[tokio::test]
async fn grpc_over_h2c_reaches_the_firestore_service() {
    let addr = start(hidane::http_routes(hidane::Admin::default())).await;
    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = FirestoreClient::new(channel);
    let status = client
        .get_document(GetDocumentRequest {
            name: "projects/demo/databases/(default)/documents/a/b".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    // A real answer from the Firestore service, not a transport-level error.
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(status.message().starts_with("Document (projects/demo/"));
}

#[tokio::test]
async fn http1_on_the_same_port_reaches_the_http_router() {
    let addr = start(hidane::http_routes(hidane::Admin::default())).await;
    let res = http1_client()
        .get(format!("http://{addr}/").parse().unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.version(), hyper::Version::HTTP_11);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Ok\n");
}

#[tokio::test]
async fn webchannel_paths_are_dispatched_by_content_type_not_path() {
    // `/google.firestore.v1.Firestore/Listen/channel` shares the gRPC service prefix. A
    // non-gRPC request there must reach the HTTP router (a WebChannel handshake), never the
    // tonic service (which would answer with a `grpc-status` header).
    let addr = start(hidane::http_routes(hidane::Admin::default())).await;
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "http://{addr}/google.firestore.v1.Firestore/Listen/channel?VER=8&RID=1"
        ))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Empty::new())
        .unwrap();
    let res = http1_client().request(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("grpc-status").is_none());
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&body).contains(r#"[[0,["c","#),
        "a session is created"
    );
}

#[tokio::test]
async fn chunked_responses_are_flushed_as_they_are_produced() {
    // A WebChannel back channel is a long-lived chunked GET; each event must reach the client
    // when it is produced, not when the response ends.
    async fn slow_stream() -> Body {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(2);
        tokio::spawn(async move {
            tx.send(Ok(Bytes::from_static(b"first\n"))).await.unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
            tx.send(Ok(Bytes::from_static(b"second\n"))).await.unwrap();
        });
        Body::from_stream(ReceiverStream::new(rx))
    }
    let addr =
        start(hidane::http_routes(hidane::Admin::default()).route("/stream", get(slow_stream)))
            .await;

    let started = Instant::now();
    let res = http1_client()
        .get(format!("http://{addr}/stream").parse().unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body();

    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let first_at = started.elapsed();
    assert_eq!(&first[..], b"first\n");
    assert!(
        first_at < Duration::from_millis(400),
        "first chunk arrived after {first_at:?}; the response is being buffered"
    );

    let second = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(&second[..], b"second\n");
    assert!(started.elapsed() >= Duration::from_millis(600));
}
