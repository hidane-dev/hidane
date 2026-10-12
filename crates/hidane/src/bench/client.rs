//! The one client both targets are measured with: the generated gRPC client over one HTTP/2
//! connection, sending the same requests in the same order.

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use hidane_proto::google::firestore::v1::{
    BeginTransactionRequest, CommitRequest, Document, GetDocumentRequest, RunQueryRequest,
    StructuredQuery, Value, Write,
    firestore_client::FirestoreClient,
    get_document_request, run_query_request,
    structured_query::{
        CollectionSelector, FieldFilter, FieldReference, Filter, field_filter, filter::FilterType,
    },
    value::ValueType,
    write::Operation,
};
use tokio_stream::StreamExt as _;
use tonic::{Code, transport::Channel};

use super::system;

/// The project every request uses, on both targets.
pub const PROJECT: &str = "demo-hidane-bench";

pub struct Client {
    inner: FirestoreClient<Channel>,
    database: String,
    addr: SocketAddr,
}

fn grpc(status: &tonic::Status) -> String {
    format!("gRPC {:?}: {}", status.code(), status.message())
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> Result<Self, String> {
        let channel = Channel::from_shared(format!("http://{addr}"))
            .map_err(|e| e.to_string())?
            .connect()
            .await
            .map_err(|e| format!("could not connect over gRPC ({e})"))?;
        Ok(Self {
            inner: FirestoreClient::new(channel)
                .max_decoding_message_size(64 << 20)
                .max_encoding_message_size(64 << 20),
            database: format!("projects/{PROJECT}/databases/(default)"),
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
