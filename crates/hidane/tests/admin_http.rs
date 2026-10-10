//! Admin HTTP endpoints (#35), checked against the behaviour measured on the official emulator
//! v1.22.0: exact paths, POST only for actions, query string ignored, newline-terminated bodies,
//! `404 Not Found` for everything else.

use std::{net::SocketAddr, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request, StatusCode};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use tokio::net::TcpListener;

async fn start(admin: hidane::Admin) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = {
        let admin = admin.clone();
        async move { admin.shutdown_requested().await }
    };
    let grpc = hidane::grpc_routes(admin.store());
    tokio::spawn(hidane::serve(
        vec![listener],
        grpc,
        hidane::http_routes(admin),
        shutdown,
    ));
    addr
}

async fn send(addr: SocketAddr, method: Method, path: &str) -> (StatusCode, Bytes) {
    let client = Client::builder(TokioExecutor::new()).build_http::<Empty<Bytes>>();
    let req = Request::builder()
        .method(method)
        .uri(format!("http://{addr}{path}"))
        .body(Empty::new())
        .unwrap();
    let res = client.request(req).await.unwrap();
    let status = res.status();
    (status, res.into_body().collect().await.unwrap().to_bytes())
}

#[tokio::test]
async fn admin_endpoints_match_the_official_emulator() {
    let addr = start(hidane::Admin::default()).await;
    let ok = |body: &'static str| (StatusCode::OK, Bytes::from_static(body.as_bytes()));
    let not_found = (StatusCode::NOT_FOUND, Bytes::from_static(b"Not Found\n"));

    for (method, path, expected) in [
        (Method::GET, "/", ok("Ok\n")),
        (Method::GET, "/?x=1", ok("Ok\n")),
        (Method::HEAD, "/", (StatusCode::NOT_FOUND, Bytes::new())),
        (Method::POST, "/", not_found.clone()),
        (Method::PUT, "/", not_found.clone()),
        (Method::GET, "/index.html", not_found.clone()),
        (Method::POST, "/reset", ok("Resetting...\n")),
        (Method::POST, "/reset?x=1", ok("Resetting...\n")),
        (Method::GET, "/reset", not_found.clone()),
        (Method::PUT, "/reset", not_found.clone()),
        (Method::DELETE, "/reset", not_found.clone()),
        (Method::POST, "/reset/", not_found.clone()),
        (Method::POST, "/RESET", not_found.clone()),
        (Method::POST, "/foo/reset", not_found.clone()),
        (Method::POST, "/emulator/v1/reset", not_found.clone()),
        (Method::POST, "/persist", not_found.clone()),
        (Method::GET, "/shutdown", not_found.clone()),
        (Method::PUT, "/shutdown", not_found.clone()),
        (Method::POST, "/shutdown/", not_found.clone()),
        (Method::POST, "/foo/shutdown", not_found.clone()),
    ] {
        assert_eq!(
            send(addr, method.clone(), path).await,
            expected,
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn post_shutdown_answers_then_stops_the_server() {
    let admin = hidane::Admin::default();
    let addr = start(admin.clone()).await;
    let waiting = tokio::spawn(async move { admin.shutdown_requested().await });

    assert_eq!(
        send(addr, Method::POST, "/shutdown?dry=1").await,
        (StatusCode::OK, Bytes::from_static(b"Shutting down...\n"))
    );
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("shutdown was signalled")
        .unwrap();

    // The listener is released: new connections are refused.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::net::TcpStream::connect(addr).await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "port still accepting"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
