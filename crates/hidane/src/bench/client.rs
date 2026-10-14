//! The one client both targets are measured with: the generated gRPC client over one HTTP/2
//! connection, sending the same requests in the same order. It sends no credential, as a
//! signed-out app does, except in `owner_writes` ([`Credential`]).

use std::{
    collections::HashSet,
    net::SocketAddr,
    time::{Duration, Instant},
};

use hidane_proto::google::firestore::v1::{
    BeginTransactionRequest, CommitRequest, Document, GetDocumentRequest, ListenRequest,
    RunQueryRequest, StructuredQuery, Target, Value, Write,
    firestore_client::FirestoreClient,
    get_document_request, listen_request,
    listen_response::ResponseType,
    run_query_request,
    structured_query::{
        CollectionSelector, FieldFilter, FieldReference, Filter, field_filter, filter::FilterType,
    },
    target::{QueryTarget, TargetType, query_target},
    target_change::TargetChangeType,
    value::ValueType,
    write::Operation,
};
use tokio::sync::{mpsc, watch};
use tokio_stream::{StreamExt as _, wrappers::ReceiverStream};
use tonic::{
    Code,
    metadata::MetadataValue,
    service::{Interceptor, interceptor::InterceptedService},
    transport::Channel,
};

use super::system;

/// The project every request uses, on both targets.
pub const PROJECT: &str = "demo-hidane-bench";

/// What a client sends as `authorization`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    /// Nothing, as a signed-out app: Security Rules apply to every request.
    None,
    /// `Bearer owner`, what the Admin SDKs send to an emulator: Security Rules do not apply.
    Owner,
}

impl Interceptor for Credential {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if *self == Self::Owner {
            request
                .metadata_mut()
                .insert("authorization", MetadataValue::from_static("Bearer owner"));
        }
        Ok(request)
    }
}

type Grpc = FirestoreClient<InterceptedService<Channel, Credential>>;

pub struct Client {
    inner: Grpc,
    database: String,
    addr: SocketAddr,
}

fn grpc(status: &tonic::Status) -> String {
    format!("gRPC {:?}: {}", status.code(), status.message())
}

async fn channel(addr: SocketAddr, credential: Credential) -> Result<Grpc, String> {
    let channel = Channel::from_shared(format!("http://{addr}"))
        .map_err(|e| e.to_string())?
        .connect()
        .await
        .map_err(|e| format!("could not connect over gRPC ({e})"))?;
    Ok(FirestoreClient::with_interceptor(channel, credential)
        .max_decoding_message_size(64 << 20)
        .max_encoding_message_size(64 << 20))
}

fn database() -> String {
    format!("projects/{PROJECT}/databases/(default)")
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> Result<Self, String> {
        Self::connect_as(addr, Credential::None).await
    }

    pub async fn connect_as(addr: SocketAddr, credential: Credential) -> Result<Self, String> {
        Ok(Self {
            inner: channel(addr, credential).await?,
            database: database(),
            addr,
        })
    }

    fn name(&self, path: &str) -> String {
        format!("{}/documents/{path}", self.database)
    }

    /// A document shaped like the Phase 0 baseline's (`{mykey, myid}`), plus `bucket` for the
    /// filtered queries.
    pub fn document(&self, path: &str, n: usize, buckets: usize) -> Document {
        let int = |v: usize| Value {
            value_type: Some(ValueType::IntegerValue(
                i64::try_from(v).unwrap_or(i64::MAX),
            )),
        };
        Document {
            name: self.name(path),
            fields: [
                (
                    "mykey".to_owned(),
                    Value {
                        value_type: Some(ValueType::StringValue(format!("key{n}"))),
                    },
                ),
                ("myid".to_owned(), int(n)),
                ("bucket".to_owned(), int(n % buckets.max(1))),
            ]
            .into_iter()
            .collect(),
            ..Document::default()
        }
    }

    pub fn update(document: Document) -> Write {
        Write {
            operation: Some(Operation::Update(document)),
            ..Write::default()
        }
    }

    pub fn delete(&self, path: &str) -> Write {
        Write {
            operation: Some(Operation::Delete(self.name(path))),
            ..Write::default()
        }
    }

    pub async fn commit(&mut self, writes: Vec<Write>) -> Result<(), String> {
        self.inner
            .commit(CommitRequest {
                database: self.database.clone(),
                writes,
                ..CommitRequest::default()
            })
            .await
            .map(|_| ())
            .map_err(|s| grpc(&s))
    }

    /// Writes `count` documents `<collection>/doc-<i>` in commits of `batch`.
    pub async fn seed(
        &mut self,
        collection: &str,
        count: usize,
        batch: usize,
        buckets: usize,
    ) -> Result<(), String> {
        let batch = batch.max(1);
        let mut start = 0;
        while start < count {
            let end = (start + batch).min(count);
            let writes = (start..end)
                .map(|i| Self::update(self.document(&format!("{collection}/doc-{i}"), i, buckets)))
                .collect();
            self.commit(writes).await?;
            start = end;
        }
        Ok(())
    }

    /// Reads one document; a missing one is `Ok(false)`.
    pub async fn get(&mut self, path: &str) -> Result<bool, String> {
        match self
            .inner
            .get_document(GetDocumentRequest {
                name: self.name(path),
                ..GetDocumentRequest::default()
            })
            .await
        {
            Ok(_) => Ok(true),
            Err(s) if s.code() == Code::NotFound => Ok(false),
            Err(s) => Err(grpc(&s)),
        }
    }

    /// `<collection> where bucket == value`, read to the end; returns the number of documents.
    pub async fn query(&mut self, collection: &str, bucket: i64) -> Result<usize, String> {
        let query = StructuredQuery {
            from: vec![CollectionSelector {
                collection_id: collection.to_owned(),
                all_descendants: false,
            }],
            r#where: Some(Filter {
                filter_type: Some(FilterType::FieldFilter(FieldFilter {
                    field: Some(FieldReference {
                        field_path: "bucket".to_owned(),
                    }),
                    op: field_filter::Operator::Equal as i32,
                    value: Some(Value {
                        value_type: Some(ValueType::IntegerValue(bucket)),
                    }),
                })),
            }),
            ..StructuredQuery::default()
        };
        let mut stream = self
            .inner
            .run_query(RunQueryRequest {
                parent: format!("{}/documents", self.database),
                query_type: Some(run_query_request::QueryType::StructuredQuery(query)),
                ..RunQueryRequest::default()
            })
            .await
            .map_err(|s| grpc(&s))?
            .into_inner();
        let mut documents = 0;
        while let Some(response) = stream.next().await {
            if response.map_err(|s| grpc(&s))?.document.is_some() {
                documents += 1;
            }
        }
        Ok(documents)
    }

    /// Read-modify-write in a transaction: begin, read the counter, write it back plus one,
    /// commit.
    pub async fn increment(&mut self, path: &str) -> Result<(), String> {
        let transaction = self
            .inner
            .begin_transaction(BeginTransactionRequest {
                database: self.database.clone(),
                ..BeginTransactionRequest::default()
            })
            .await
            .map_err(|s| grpc(&s))?
            .into_inner()
            .transaction;
        let current = match self
            .inner
            .get_document(GetDocumentRequest {
                name: self.name(path),
                consistency_selector: Some(get_document_request::ConsistencySelector::Transaction(
                    transaction.clone(),
                )),
                ..GetDocumentRequest::default()
            })
            .await
        {
            Ok(response) => match response.into_inner().fields.get("myid") {
                Some(Value {
                    value_type: Some(ValueType::IntegerValue(n)),
                }) => usize::try_from(*n).unwrap_or(0),
                _ => 0,
            },
            Err(s) if s.code() == Code::NotFound => 0,
            Err(s) => return Err(grpc(&s)),
        };
        self.inner
            .commit(CommitRequest {
                database: self.database.clone(),
                writes: vec![Self::update(self.document(path, current + 1, 1))],
                transaction,
                ..CommitRequest::default()
            })
            .await
            .map(|_| ())
            .map_err(|s| grpc(&s))
    }

    /// Clears every document, as test suites do between tests (`clearFirestore()`):
    /// `DELETE /emulator/v1/projects/{project}/databases/(default)/documents`.
    pub fn reset(&self) -> Result<(), String> {
        let path = format!("/emulator/v1/projects/{PROJECT}/databases/(default)/documents");
        match system::http(self.addr, "DELETE", &path, &[], Duration::from_secs(30)) {
            Ok((200, _)) => Ok(()),
            Ok((status, _)) => Err(format!("reset answered HTTP {status}")),
            Err(e) => Err(format!("reset failed ({e})")),
        }
    }
}

/// A consistent snapshot of a listened collection: how many documents it held, and when it
/// arrived.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub documents: usize,
    pub at: Instant,
}

/// A Listen stream on its own connection, with one target on a whole collection, read in the
/// background as it arrives. It keeps the target's documents as the SDKs do: a `RESET` empties
/// them until the target is `CURRENT` again, and each `NO_CHANGE` for all targets after that is a
/// consistent snapshot.
pub struct Listener {
    /// Kept open: the stream lasts while the sender does.
    _requests: mpsc::Sender<ListenRequest>,
    snapshots: watch::Receiver<Option<Snapshot>>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

const TARGET_ID: i32 = 1;

impl Listener {
    pub async fn start(addr: SocketAddr, collection: &str) -> Result<Self, String> {
        let mut inner = channel(addr, Credential::None).await?;
        let database = database();
        let (requests, rx) = mpsc::channel(1);
        let query = StructuredQuery {
            from: vec![CollectionSelector {
                collection_id: collection.to_owned(),
                all_descendants: false,
            }],
            ..StructuredQuery::default()
        };
        requests
            .send(ListenRequest {
                database: database.clone(),
                target_change: Some(listen_request::TargetChange::AddTarget(Target {
                    target_id: TARGET_ID,
                    target_type: Some(TargetType::Query(QueryTarget {
                        parent: format!("{database}/documents"),
                        query_type: Some(query_target::QueryType::StructuredQuery(query)),
                    })),
                    ..Target::default()
                })),
                ..ListenRequest::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        // The SDKs name the database in this header; the official emulator's Listen needs it.
        let mut request = tonic::Request::new(ReceiverStream::new(rx));
        request.metadata_mut().insert(
            "google-cloud-resource-prefix",
            database.parse().map_err(|_| "bad database name")?,
        );
        let mut stream = inner
            .listen(request)
            .await
            .map_err(|s| format!("Listen: {}", grpc(&s)))?
            .into_inner();
        let (tx, snapshots) = watch::channel(None);
        let task = tokio::spawn(async move {
            let mut documents = HashSet::new();
            let mut current = false;
            while let Some(response) = stream.next().await {
                match response
                    .map_err(|s| format!("Listen: {}", grpc(&s)))?
                    .response_type
                {
                    Some(ResponseType::TargetChange(change)) => {
                        let ours =
                            change.target_ids.is_empty() || change.target_ids.contains(&TARGET_ID);
                        match TargetChangeType::try_from(change.target_change_type) {
                            Ok(TargetChangeType::Reset) if ours => {
                                documents.clear();
                                current = false;
                            }
                            Ok(TargetChangeType::Current) if ours => current = true,
                            Ok(TargetChangeType::NoChange)
                                if change.target_ids.is_empty() && current =>
                            {
                                tx.send_replace(Some(Snapshot {
                                    documents: documents.len(),
                                    at: Instant::now(),
                                }));
                            }
                            Ok(TargetChangeType::Remove) if ours => {
                                return Err(format!(
                                    "the server removed the Listen target ({})",
                                    change.cause.map_or_else(String::new, |c| c.message)
                                ));
                            }
                            _ => {}
                        }
                    }
                    Some(ResponseType::DocumentChange(change)) => {
                        if let Some(document) = change.document {
                            if change.target_ids.contains(&TARGET_ID) {
                                documents.insert(document.name);
                            } else if change.removed_target_ids.contains(&TARGET_ID) {
                                documents.remove(&document.name);
                            }
                        }
                    }
                    Some(ResponseType::DocumentDelete(delete)) => {
                        documents.remove(&delete.document);
                    }
                    Some(ResponseType::DocumentRemove(remove)) => {
                        documents.remove(&remove.document);
                    }
                    Some(ResponseType::Filter(_)) | None => {}
                }
            }
            Err("the Listen stream ended".to_owned())
        });
        Ok(Self {
            _requests: requests,
            snapshots,
            task,
        })
    }

    /// The first consistent snapshot holding at least `documents` documents.
    pub async fn wait_for(
        &mut self,
        documents: usize,
        timeout: Duration,
    ) -> Result<Snapshot, String> {
        let wait = self
            .snapshots
            .wait_for(|s| s.is_some_and(|s| s.documents >= documents));
        match tokio::time::timeout(timeout, wait).await {
            Ok(Ok(snapshot)) => (*snapshot).ok_or_else(|| "no snapshot".to_owned()),
            Ok(Err(_)) => Err(match (&mut self.task).await {
                Ok(Err(e)) => e,
                Ok(Ok(())) => "the Listen stream ended".to_owned(),
                Err(e) => format!("the listener failed ({e})"),
            }),
            Err(_) => Err(format!(
                "no snapshot with {documents} documents within {} s",
                timeout.as_secs()
            )),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The synthetic CI workload's tests, as a test suite would run them: each writes 3 documents
/// in one commit, reads 2, runs a query, and clears the database through the reset endpoint.
pub async fn ci_tests(client: &mut Client, tests: usize) -> Result<(), String> {
    for test in 0..tests {
        let writes = (0..3)
            .map(|k| Client::update(client.document(&format!("items/t{test}-{k}"), k, 3)))
            .collect();
        client.commit(writes).await?;
        client.get(&format!("items/t{test}-0")).await?;
        client.get(&format!("items/t{test}-2")).await?;
        client.query("items", 1).await?;
        client.reset()?;
    }
    Ok(())
}

/// Time from `started` until a gRPC `GetDocument` of a missing document is answered, retrying
/// until the server is up.
pub async fn first_request(addr: SocketAddr, started: Instant) -> Result<Duration, String> {
    let deadline = Duration::from_secs(120);
    loop {
        let last = match Client::connect(addr).await {
            Ok(mut client) => match client.get("bench/missing").await {
                Ok(_) => return Ok(started.elapsed()),
                Err(e) => e,
            },
            Err(e) => e,
        };
        if started.elapsed() > deadline {
            return Err(format!("no gRPC answer within 120 s ({last})"));
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owner_credential_sends_authorization() {
        let sent = |mut credential: Credential| {
            credential
                .call(tonic::Request::new(()))
                .unwrap()
                .metadata()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
        };
        assert_eq!(sent(Credential::Owner).as_deref(), Some("Bearer owner"));
        assert_eq!(sent(Credential::None), None);
    }
}
