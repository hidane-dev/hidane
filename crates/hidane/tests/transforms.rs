//! Field transforms (#23), replaying every case the official emulator answered in
//! `fixtures/transforms.json` (`tools/oracle/transforms.py`) over gRPC.

mod common;

use std::net::SocketAddr;

use common::protojson;
use hidane_proto::google::firestore::v1::{
    GetDocumentRequest, firestore_client::FirestoreClient, value::ValueType,
};
use tokio::net::TcpListener;
use tonic::transport::Channel;

async fn start() -> FirestoreClient<Channel> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(admin.store()),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    FirestoreClient::new(channel)
}

#[tokio::test]
async fn every_recorded_case_gives_the_official_answer() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/transforms.json")).unwrap();
    let mut client = start().await;
    let mut steps = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        for step in case["steps"].as_array().unwrap() {
            steps += 1;
            let path = step["path"].as_str().unwrap();
            let expected = &step["response"];
            if step["method"] == "GET" {
                let doc = client
                    .get_document(GetDocumentRequest {
                        name: path.to_owned(),
                        ..GetDocumentRequest::default()
                    })
                    .await
                    .unwrap_or_else(|e| panic!("{name}: GET {path}: {e}"))
                    .into_inner();
                let want = protojson::fields(&expected["fields"]);
                assert!(
                    protojson::same_fields(&doc.fields, &want),
                    "{name}: GET {path}\n  hidane:   {:?}\n  official: {want:?}",
                    doc.fields
                );
                continue;
            }
            assert!(path.ends_with(":commit"), "{name}: unexpected step {path}");
            let got = client
                .commit(protojson::commit_request(path, &step["body"]))
                .await;
            if let Some(error) = expected.get("error") {
                let err = got.expect_err(name);
                assert_eq!(
                    err.code(),
                    protojson::code(error["status"].as_str().unwrap()),
                    "{name}"
                );
                assert_eq!(err.message(), error["message"].as_str().unwrap(), "{name}");
                continue;
            }
            let response = got.unwrap_or_else(|e| panic!("{name}: {e}")).into_inner();
            let commit_time = response.commit_time.unwrap();
            let request_time = hidane_core::transform::request_time(&commit_time);
            let want = expected["writeResults"].as_array().unwrap();
            assert_eq!(response.write_results.len(), want.len(), "{name}");
            for (result, want) in response.write_results.iter().zip(want) {
                let want: Vec<_> = want["transformResults"]
                    .as_array()
                    .map(|vs| vs.iter().map(protojson::value).collect())
                    .unwrap_or_default();
                assert_eq!(result.transform_results.len(), want.len(), "{name}");
                for (got, want) in result.transform_results.iter().zip(&want) {
                    assert!(
                        protojson::same(got, want),
                        "{name}: {got:?} vs official {want:?}"
                    );
                    // Server timestamps: the commit time truncated to milliseconds.
                    if let Some(ValueType::TimestampValue(ts)) = &got.value_type {
                        assert_eq!(ts, &request_time, "{name}");
                    }
                }
            }
        }
    }
    assert!(steps > 30, "replayed only {steps} steps");
}

#[tokio::test]
async fn noop_transforms_keep_update_time() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/transforms.json")).unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "transform that changes nothing")
        .unwrap();
    let mut client = start().await;
    let steps = case["steps"].as_array().unwrap();
    let seed = client
        .commit(protojson::commit_request(
            steps[0]["path"].as_str().unwrap(),
            &steps[0]["body"],
        ))
        .await
        .unwrap()
        .into_inner();
    let noop = client
        .commit(protojson::commit_request(
            steps[1]["path"].as_str().unwrap(),
            &steps[1]["body"],
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(noop.write_results[0].update_time, seed.commit_time);
}
