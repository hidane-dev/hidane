//! Commits that write one document more than once (#74), replaying every case the official
//! emulator answered in `fixtures/same_document_writes.json`
//! (`tools/oracle/same_document_writes.py`) over gRPC: which sequences are refused, with which
//! message, and the document each leaves. Precondition failures compare their codes only: the
//! official emulator's messages print its internal Datastore key (docs/parity-exceptions.md).

mod common;

use common::protojson;
use hidane_proto::google::firestore::v1::{GetDocumentRequest, firestore_client::FirestoreClient};
use tokio::net::TcpListener;
use tonic::{Code, transport::Channel};

async fn start() -> FirestoreClient<Channel> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
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
        serde_json::from_str(include_str!("fixtures/same_document_writes.json")).unwrap();
    let mut client = start().await;
    let mut refused = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        for step in case["steps"].as_array().unwrap() {
            let path = step["path"].as_str().unwrap();
            let expected = &step["response"];
            if step["method"] == "GET" {
                let got = client
                    .get_document(GetDocumentRequest {
                        name: path.to_owned(),
                        ..GetDocumentRequest::default()
                    })
                    .await;
                if step["status"] != 200 {
                    let err = got.expect_err(name);
                    assert_eq!(err.code(), Code::NotFound, "{name}: GET {path}");
                    continue;
                }
                let doc = got
                    .unwrap_or_else(|e| panic!("{name}: GET {path}: {e}"))
                    .into_inner();
                let want = protojson::fields(&expected["fields"]);
                assert!(
                    protojson::same_fields(&doc.fields, &want),
                    "{name}: GET {path}\n  hidane:   {:?}\n  official: {want:?}",
                    doc.fields
                );
                let order = |fields: &hidane_proto::ordered::Fields| {
                    fields.keys().cloned().collect::<Vec<_>>()
                };
                assert_eq!(order(&doc.fields), order(&want), "{name}: field order");
                continue;
            }
            assert!(path.ends_with(":commit"), "{name}: unexpected step {path}");
            let got = client
                .commit(protojson::commit_request(path, &step["body"]))
                .await;
            let Some(error) = expected.get("error") else {
                let response = got.unwrap_or_else(|e| panic!("{name}: {e}")).into_inner();
                let writes = step["body"]["writes"].as_array().unwrap();
                assert_eq!(response.write_results.len(), writes.len(), "{name}");
                continue;
            };
            let err = got.expect_err(name);
            let code = protojson::code(error["status"].as_str().unwrap());
            assert_eq!(err.code(), code, "{name}: {}", err.message());
            if !matches!(code, Code::NotFound | Code::AlreadyExists) {
                assert_eq!(err.message(), error["message"].as_str().unwrap(), "{name}");
            }
            if err.message().starts_with("Cannot ") {
                refused += 1;
            }
        }
    }
    assert!(refused > 40, "only {refused} refused sequences replayed");
}
