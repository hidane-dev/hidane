//! Document RPCs against the official emulator's recorded behaviour
//! (`fixtures/document_writes.json`, from `tools/oracle/document_writes.py`). Each test replays
//! one recorded case over gRPC: status codes always match; messages match where hidane uses
//! the official wording (docs/parity-exceptions.md lists the exceptions).

use std::{collections::BTreeMap, net::SocketAddr};

use hidane_proto::google::firestore::v1::{
    ArrayValue, BatchGetDocumentsRequest, BatchWriteRequest, CommitRequest, CreateDocumentRequest,
    Document, DocumentMask, GetDocumentRequest, ListCollectionIdsRequest, ListDocumentsRequest,
    MapValue, Precondition, Value, Write, batch_get_documents_response,
    firestore_client::FirestoreClient, get_document_request, precondition::ConditionType,
    value::ValueType, write::Operation,
};
use prost_types::Timestamp;
use tokio::net::TcpListener;
use tonic::{Code, Request, Status, transport::Channel};

type Client = FirestoreClient<Channel>;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/document_writes.json")).unwrap()
}

/// The recorded error message of step `step` of case `name`.
fn official_message(name: &str, step: usize) -> String {
    let fixture = fixture();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no case {name}"));
    case["steps"][step]["response"]["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn start() -> (Client, hidane::Admin, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin = hidane::Admin::default();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin.clone()),
        std::future::pending(),
    ));
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    (FirestoreClient::new(channel), admin, addr)
}

fn as_admin<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", "Bearer owner".parse().unwrap());
    request
}

fn db(project: &str) -> String {
    format!("projects/{project}/databases/(default)")
}

fn doc(project: &str, path: &str) -> String {
    format!("{}/documents/{path}", db(project))
}

fn int(i: i64) -> Value {
    Value {
        value_type: Some(ValueType::IntegerValue(i)),
    }
}

fn map(entries: &[(&str, Value)]) -> Value {
    Value {
        value_type: Some(ValueType::MapValue(MapValue {
            fields: fields(entries),
        })),
    }
}

fn fields(entries: &[(&str, Value)]) -> BTreeMap<String, Value> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn update(name: &str, entries: &[(&str, Value)]) -> Write {
    Write {
        operation: Some(Operation::Update(Document {
            name: name.to_owned(),
            fields: fields(entries),
            ..Document::default()
        })),
        ..Write::default()
    }
}

fn with_exists(mut write: Write, exists: bool) -> Write {
    write.current_document = Some(Precondition {
        condition_type: Some(ConditionType::Exists(exists)),
    });
    write
}

fn delete(name: &str) -> Write {
    Write {
        operation: Some(Operation::Delete(name.to_owned())),
        ..Write::default()
    }
}

async fn commit(
    client: &mut Client,
    project: &str,
    writes: Vec<Write>,
) -> Result<hidane_proto::google::firestore::v1::CommitResponse, Status> {
    client
        .commit(CommitRequest {
            database: db(project),
            writes,
            ..CommitRequest::default()
        })
        .await
        .map(tonic::Response::into_inner)
}

async fn get(client: &mut Client, name: &str) -> Result<Document, Status> {
    client
        .get_document(GetDocumentRequest {
            name: name.to_owned(),
            ..GetDocumentRequest::default()
        })
        .await
        .map(tonic::Response::into_inner)
}

#[tokio::test]
async fn reads_and_their_errors() {
    let (mut c, _, _) = start().await;
    let err = get(&mut c, &doc("p-get-missing", "c/d")).await.unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(err.message(), official_message("get missing document", 0));

    let err = get(&mut c, "projects/p-grpc/databases/(default)/documents/c")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "Document name \"projects/p-grpc/databases/(default)/documents/c\" lacks \"/\" at index 47."
    );

    commit(
        &mut c,
        "p-get-mask",
        vec![update(
            &doc("p-get-mask", "c/d"),
            &[
                ("a", int(1)),
                ("b", map(&[("x", int(1)), ("y", int(2))])),
                ("c", int(3)),
            ],
        )],
    )
    .await
    .unwrap();
    let masked = c
        .get_document(GetDocumentRequest {
            name: doc("p-get-mask", "c/d"),
            mask: Some(DocumentMask {
                field_paths: vec!["b.x".into(), "c".into()],
            }),
            ..GetDocumentRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        masked.fields,
        fields(&[("b", map(&[("x", int(1))])), ("c", int(3))])
    );
}

#[tokio::test]
async fn read_time_follows_the_official_emulator() {
    let (mut c, _, _) = start().await;
    let commit_time = commit(
        &mut c,
        "p-grpc",
        vec![update(&doc("p-grpc", "c/d"), &[("a", int(1))])],
    )
    .await
    .unwrap()
    .commit_time
    .unwrap();
    let at = |ts: Timestamp| GetDocumentRequest {
        name: doc("p-grpc", "c/d"),
        consistency_selector: Some(get_document_request::ConsistencySelector::ReadTime(ts)),
        ..GetDocumentRequest::default()
    };
    let shifted = |micros: i64| {
        let total = commit_time.seconds * 1_000_000 + i64::from(commit_time.nanos) / 1_000 + micros;
        Timestamp {
            seconds: total.div_euclid(1_000_000),
            nanos: i32::try_from(total.rem_euclid(1_000_000) * 1_000).unwrap(),
        }
    };
    assert!(c.get_document(at(commit_time)).await.is_ok());
    assert_eq!(
        c.get_document(at(shifted(-1))).await.unwrap_err().code(),
        Code::NotFound
    );
    let future = c
        .get_document(at(shifted(3_600_000_000)))
        .await
        .unwrap_err();
    assert_eq!(
        (future.code(), future.message()),
        (
            Code::InvalidArgument,
            "The requested 'read_time' cannot be in the future."
        )
    );
    let old = c
        .get_document(at(Timestamp {
            seconds: 1,
            nanos: 0,
        }))
        .await
        .unwrap_err();
    assert_eq!(
        (old.code(), old.message()),
        (
            Code::FailedPrecondition,
            "The requested 'read_time' is too old."
        )
    );
}

#[tokio::test]
async fn preconditions() {
    let (mut c, _, _) = start().await;

    let created = c
        .create_document(CreateDocumentRequest {
            parent: format!("{}/documents", db("p-create-twice")),
            collection_id: "c".into(),
            document_id: "d".into(),
            document: Some(Document {
                fields: fields(&[("a", int(1))]),
                ..Document::default()
            }),
            ..CreateDocumentRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(created.create_time, created.update_time);
    let again = c
        .create_document(CreateDocumentRequest {
            parent: format!("{}/documents", db("p-create-twice")),
            collection_id: "c".into(),
            document_id: "d".into(),
            ..CreateDocumentRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(again.code(), Code::AlreadyExists);
    assert_eq!(
        again.message(),
        format!("Document already exists: {}", doc("p-create-twice", "c/d"))
    );

    let auto = c
        .create_document(CreateDocumentRequest {
            parent: format!("{}/documents", db("p-create-auto")),
            collection_id: "c".into(),
            ..CreateDocumentRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let id = auto.name.rsplit('/').next().unwrap();
    assert_eq!(id.len(), 20);
    assert!(id.chars().all(|ch| ch.is_ascii_alphanumeric()));

    let err = commit(
        &mut c,
        "p-upd-missing",
        vec![with_exists(
            update(&doc("p-upd-missing", "c/d"), &[("a", int(1))]),
            true,
        )],
    )
    .await
    .unwrap_err();
    assert_eq!(
        (err.code(), err.message().to_owned()),
        (
            Code::NotFound,
            format!("No document to update: {}", doc("p-upd-missing", "c/d"))
        )
    );

    commit(
        &mut c,
        "p-upd-existing",
        vec![update(&doc("p-upd-existing", "c/d"), &[("a", int(1))])],
    )
    .await
    .unwrap();
    let err = commit(
        &mut c,
        "p-upd-existing",
        vec![with_exists(
            update(&doc("p-upd-existing", "c/d"), &[("a", int(2))]),
            false,
        )],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::AlreadyExists);

    commit(
        &mut c,
        "p-upd-time",
        vec![update(&doc("p-upd-time", "c/d"), &[("a", int(1))])],
    )
    .await
    .unwrap();
    let mut stale = update(&doc("p-upd-time", "c/d"), &[("a", int(2))]);
    stale.current_document = Some(Precondition {
        condition_type: Some(ConditionType::UpdateTime(Timestamp {
            seconds: 978_307_200,
            nanos: 0,
        })),
    });
    let err = commit(&mut c, "p-upd-time", vec![stale]).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let official = official_message("update_time precondition", 1);
    let shape = |m: &str| -> Vec<String> {
        m.split('(')
            .map(|part| {
                part.trim_start_matches(|ch: char| ch.is_ascii_digit())
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(
        shape(err.message()),
        shape(&official),
        "{} vs {official}",
        err.message()
    );
    assert!(err.message().ends_with("(978307200000000)"));

    let ok = commit(
        &mut c,
        "p-upd-time-ok",
        vec![update(&doc("p-upd-time-ok", "c/d"), &[("a", int(1))])],
    )
    .await
    .unwrap();
    let mut fresh = update(&doc("p-upd-time-ok", "c/d"), &[("a", int(2))]);
    fresh.current_document = Some(Precondition {
        condition_type: Some(ConditionType::UpdateTime(ok.commit_time.unwrap())),
    });
    commit(&mut c, "p-upd-time-ok", vec![fresh]).await.unwrap();

    let err = commit(
        &mut c,
        "p-del-missing",
        vec![with_exists(delete(&doc("p-del-missing", "c/d")), true)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let plain = commit(
        &mut c,
        "p-del-plain",
        vec![delete(&doc("p-del-plain", "c/d"))],
    )
    .await
    .unwrap();
    assert_eq!(plain.write_results.len(), 1);
    assert!(plain.write_results[0].update_time.is_none());
}

#[tokio::test]
async fn write_semantics() {
    let (mut c, _, _) = start().await;

    // Writing what is stored keeps update_time; the commit still gets a new time.
    let first = commit(
        &mut c,
        "p-identical",
        vec![update(&doc("p-identical", "c/d"), &[("a", int(1))])],
    )
    .await
    .unwrap();
    let second = commit(
        &mut c,
        "p-identical",
        vec![update(&doc("p-identical", "c/d"), &[("a", int(1))])],
    )
    .await
    .unwrap();
    assert_eq!(
        second.write_results[0].update_time,
        first.write_results[0].update_time
    );
    assert_ne!(second.commit_time, first.commit_time);
    assert_eq!(
        get(&mut c, &doc("p-identical", "c/d"))
            .await
            .unwrap()
            .update_time,
        first.commit_time
    );

    commit(
        &mut c,
        "p-mask",
        vec![update(
            &doc("p-mask", "c/d"),
            &[
                ("a", int(1)),
                ("b", int(2)),
                ("c", map(&[("x", int(1)), ("y", int(2))])),
            ],
        )],
    )
    .await
    .unwrap();
    let mut patch = update(
        &doc("p-mask", "c/d"),
        &[("a", int(10)), ("c", map(&[("x", int(5))])), ("e", int(9))],
    );
    patch.update_mask = Some(DocumentMask {
        field_paths: vec!["a".into(), "c.x".into(), "d".into()],
    });
    commit(&mut c, "p-mask", vec![patch]).await.unwrap();
    assert_eq!(
        get(&mut c, &doc("p-mask", "c/d")).await.unwrap().fields,
        fields(&[
            ("a", int(10)),
            ("b", int(2)),
            ("c", map(&[("x", int(5)), ("y", int(2))]))
        ])
    );

    let mut on_missing = update(
        &doc("p-mask-missing", "c/d"),
        &[("a", int(1)), ("b", map(&[("x", int(1))]))],
    );
    on_missing.update_mask = Some(DocumentMask {
        field_paths: vec!["b.x".into()],
    });
    commit(&mut c, "p-mask-missing", vec![on_missing])
        .await
        .unwrap();
    assert_eq!(
        get(&mut c, &doc("p-mask-missing", "c/d"))
            .await
            .unwrap()
            .fields,
        fields(&[("b", map(&[("x", int(1))]))])
    );

    let twice = commit(
        &mut c,
        "p-twice",
        vec![
            update(&doc("p-twice", "c/d"), &[("a", int(1))]),
            update(&doc("p-twice", "c/d"), &[("a", int(2))]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(twice.write_results.len(), 2);
    assert_eq!(
        get(&mut c, &doc("p-twice", "c/d")).await.unwrap().fields,
        fields(&[("a", int(2))])
    );

    let empty = commit(&mut c, "p-empty", vec![]).await.unwrap();
    assert!(empty.commit_time.is_none() && empty.write_results.is_empty());
}

#[tokio::test]
async fn invalid_writes_use_the_official_messages() {
    let (mut c, _, _) = start().await;
    let nested = Value {
        value_type: Some(ValueType::ArrayValue(ArrayValue {
            values: vec![Value {
                value_type: Some(ValueType::ArrayValue(ArrayValue::default())),
            }],
        })),
    };
    let mut bad_mask = update(&doc("p-bad-mask", "c/d"), &[("a", int(1))]);
    bad_mask.update_mask = Some(DocumentMask {
        field_paths: vec!["a..b".into()],
    });
    for (case, project, write) in [
        (
            "reserved document id",
            "p-reserved-id",
            update(&doc("p-reserved-id", "c/__x__"), &[("a", int(1))]),
        ),
        (
            "dot document id",
            "p-dot-id",
            update(&doc("p-dot-id", "c/."), &[("a", int(1))]),
        ),
        (
            "reserved field name",
            "p-reserved-field",
            update(&doc("p-reserved-field", "c/d"), &[("__x__", int(1))]),
        ),
        (
            "nested array",
            "p-nested",
            update(&doc("p-nested", "c/d"), &[("a", nested.clone())]),
        ),
        ("invalid mask path", "p-bad-mask", bad_mask.clone()),
    ] {
        let err = commit(&mut c, project, vec![write]).await.unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument, "{case}");
        assert_eq!(err.message(), official_message(case, 0), "{case}");
    }
    let err = commit(
        &mut c,
        "p-numeric",
        vec![update(&doc("p-numeric", "c/__id5x__"), &[("a", int(1))])],
    )
    .await
    .unwrap_err();
    assert_eq!(err.message(), official_message("numeric id document", 2));
    commit(
        &mut c,
        "p-numeric",
        vec![update(&doc("p-numeric", "c/__id5__"), &[("a", int(1))])],
    )
    .await
    .unwrap();

    let err = c
        .commit(CommitRequest {
            database: db("p"),
            transaction: vec![1, 2],
            ..CommitRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        (err.code(), err.message()),
        (Code::InvalidArgument, "Invalid transaction.")
    );
}

#[tokio::test]
async fn batch_get_keeps_request_order_and_one_read_time() {
    let (mut c, _, _) = start().await;
    commit(
        &mut c,
        "p-batchget",
        vec![
            update(&doc("p-batchget", "c/a"), &[("n", int(1))]),
            update(&doc("p-batchget", "c/b"), &[("n", int(2))]),
        ],
    )
    .await
    .unwrap();
    let mut stream = c
        .batch_get_documents(BatchGetDocumentsRequest {
            database: db("p-batchget"),
            documents: vec![
                doc("p-batchget", "c/b"),
                doc("p-batchget", "c/missing"),
                doc("p-batchget", "c/a"),
            ],
            ..BatchGetDocumentsRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut seen = Vec::new();
    let mut read_times = Vec::new();
    while let Some(item) = stream.message().await.unwrap() {
        read_times.push(item.read_time);
        seen.push(match item.result.unwrap() {
            batch_get_documents_response::Result::Found(d) => {
                format!("found:{}", d.name.rsplit('/').next().unwrap())
            }
            batch_get_documents_response::Result::Missing(n) => {
                format!("missing:{}", n.rsplit('/').next().unwrap())
            }
        });
    }
    assert_eq!(seen, ["found:b", "missing:missing", "found:a"]);
    read_times.dedup();
    assert_eq!(read_times.len(), 1);
}

#[tokio::test]
async fn listing_documents_and_collections() {
    let (mut c, _, _) = start().await;
    commit(
        &mut c,
        "p-list",
        vec![
            update(&doc("p-list", "c/a"), &[("n", int(1))]),
            update(&doc("p-list", "c/b"), &[("n", int(2))]),
            update(&doc("p-list", "c/c"), &[("n", int(3))]),
            update(&doc("p-list", "c/ghost/sub/x"), &[("n", int(4))]),
        ],
    )
    .await
    .unwrap();
    let list = |page_size, page_token: String, show_missing| ListDocumentsRequest {
        parent: format!("{}/documents", db("p-list")),
        collection_id: "c".into(),
        page_size,
        page_token,
        show_missing,
        ..ListDocumentsRequest::default()
    };
    let names = |docs: &[Document]| {
        docs.iter()
            .map(|d| d.name.rsplit('/').next().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    let all = c
        .list_documents(list(0, String::new(), false))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&all.documents), ["a", "b", "c"]);
    assert!(all.next_page_token.is_empty());

    let page1 = c
        .list_documents(list(2, String::new(), false))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&page1.documents), ["a", "b"]);
    let page2 = c
        .list_documents(list(2, page1.next_page_token, false))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&page2.documents), ["c"]);
    assert!(page2.next_page_token.is_empty());

    let denied = c
        .list_documents(list(0, String::new(), true))
        .await
        .unwrap_err();
    assert_eq!(
        (denied.code(), denied.message().to_owned()),
        (
            Code::PermissionDenied,
            official_message("list documents", 3)
        )
    );
    let with_missing = c
        .list_documents(as_admin(list(0, String::new(), true)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&with_missing.documents), ["a", "b", "c", "ghost"]);
    let ghost = &with_missing.documents[3];
    assert!(ghost.fields.is_empty() && ghost.create_time.is_none());

    commit(
        &mut c,
        "p-ids",
        vec![
            update(&doc("p-ids", "users/a"), &[("n", int(1))]),
            update(&doc("p-ids", "users/a/posts/p"), &[("n", int(1))]),
            update(&doc("p-ids", "ghost/g/x/y"), &[("n", int(1))]),
        ],
    )
    .await
    .unwrap();
    let ids = |parent: String, page_size, page_token: String| ListCollectionIdsRequest {
        parent,
        page_size,
        page_token,
        ..ListCollectionIdsRequest::default()
    };
    let root = format!("{}/documents", db("p-ids"));
    let denied = c
        .list_collection_ids(ids(root.clone(), 0, String::new()))
        .await
        .unwrap_err();
    assert_eq!(
        (denied.code(), denied.message().to_owned()),
        (
            Code::PermissionDenied,
            official_message("list collection ids", 1)
        )
    );
    let all = c
        .list_collection_ids(as_admin(ids(root.clone(), 0, String::new())))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(all.collection_ids, ["ghost", "users"]);
    let sub = c
        .list_collection_ids(as_admin(ids(doc("p-ids", "users/a"), 0, String::new())))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sub.collection_ids, ["posts"]);
    let first = c
        .list_collection_ids(as_admin(ids(root.clone(), 1, String::new())))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first.collection_ids, ["ghost"]);
    let next = c
        .list_collection_ids(as_admin(ids(root, 1, first.next_page_token)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(next.collection_ids, ["users"]);
    assert!(next.next_page_token.is_empty());
}

#[tokio::test]
async fn batch_write_reports_each_write() {
    let (mut c, _, _) = start().await;
    commit(
        &mut c,
        "p-batchwrite2",
        vec![update(&doc("p-batchwrite2", "c/exists"), &[("n", int(1))])],
    )
    .await
    .unwrap();
    let request = || BatchWriteRequest {
        database: db("p-batchwrite2"),
        writes: vec![
            update(&doc("p-batchwrite2", "c/new"), &[("n", int(1))]),
            with_exists(
                update(&doc("p-batchwrite2", "c/missing"), &[("n", int(1))]),
                true,
            ),
            with_exists(
                update(&doc("p-batchwrite2", "c/exists"), &[("n", int(2))]),
                false,
            ),
            delete(&doc("p-batchwrite2", "c/other")),
        ],
        ..BatchWriteRequest::default()
    };
    let denied = c.batch_write(request()).await.unwrap_err();
    assert_eq!(
        (denied.code(), denied.message().to_owned()),
        (
            Code::PermissionDenied,
            official_message("batch write statuses", 1)
        )
    );

    let response = c
        .batch_write(as_admin(request()))
        .await
        .unwrap()
        .into_inner();
    let codes: Vec<i32> = response.status.iter().map(|s| s.code).collect();
    assert_eq!(
        codes,
        [0, Code::NotFound as i32, Code::AlreadyExists as i32, 0]
    );
    assert!(response.write_results[0].update_time.is_some());
    assert!(response.write_results[1].update_time.is_none());
    assert!(get(&mut c, &doc("p-batchwrite2", "c/new")).await.is_ok());

    let mut duplicate = request();
    duplicate
        .writes
        .push(delete(&doc("p-batchwrite2", "c/new")));
    let err = c.batch_write(as_admin(duplicate)).await.unwrap_err();
    assert_eq!(
        (err.code(), err.message().to_owned()),
        (Code::InvalidArgument, official_message("batch write", 1))
    );
}

#[tokio::test]
async fn databases_are_separate_and_reset_clears_everything() {
    let (mut c, admin, _) = start().await;
    let named = "projects/p-named/databases/second";
    c.commit(CommitRequest {
        database: named.into(),
        writes: vec![update(&format!("{named}/documents/c/d"), &[("a", int(1))])],
        ..CommitRequest::default()
    })
    .await
    .unwrap();
    assert!(get(&mut c, &format!("{named}/documents/c/d")).await.is_ok());
    assert_eq!(
        get(&mut c, &doc("p-named", "c/d"))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );

    admin.store().clear();
    assert_eq!(
        get(&mut c, &format!("{named}/documents/c/d"))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
}
