//! `google.firestore.v1.Firestore`.
//!
//! Implemented: GetDocument, ListDocuments, CreateDocument, UpdateDocument, DeleteDocument,
//! BatchGetDocuments, BeginTransaction, Commit, Rollback, RunQuery, RunAggregationQuery, Write,
//! Listen, BatchWrite, ListCollectionIds. The rest answer `UNIMPLEMENTED` through the generated default stubs
//! until their issues land.
//!
//! Behaviour follows the official emulator as recorded in `tests/fixtures/document_writes.json`
//! (`tools/oracle/document_writes.py`) and `tests/fixtures/transactions.json`
//! (`tools/oracle/transactions.py`), `tests/fixtures/queries.json` (`tools/oracle/queries.py`)
//! and `tests/fixtures/aggregations.json` (`tools/oracle/aggregations.py`), including its error messages, except where the official
//! message prints internal Datastore keys (docs/parity-exceptions.md).

mod aggregation;
pub(crate) mod auth;
pub(crate) mod changes;
mod clear;
mod listen;
mod names;
mod query;
pub(crate) mod transactions;
mod validate;
mod write_stream;
mod writes;

use std::{
    collections::HashSet,
    hash::{BuildHasher, Hasher},
    ops::ControlFlow,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hidane_core::{
    field_path::{FieldPath, Fields, project},
    key,
    path::ResourcePath,
    store::{ListItem, ListOptions, ReadTime, Store, StoreError, StoredDocument},
};
use hidane_proto::google::{
    firestore::v1::{
        AggregationResult, BatchGetDocumentsRequest, BatchGetDocumentsResponse, BatchWriteRequest,
        BatchWriteResponse, BeginTransactionRequest, BeginTransactionResponse, CommitRequest,
        CommitResponse, CreateDocumentRequest, DeleteDocumentRequest, Document, DocumentMask,
        ExecutePipelineRequest, ExecutePipelineResponse, GetDocumentRequest,
        ListCollectionIdsRequest, ListCollectionIdsResponse, ListDocumentsRequest,
        ListDocumentsResponse, ListenRequest, ListenResponse, PartitionQueryRequest,
        PartitionQueryResponse, Precondition, RollbackRequest, RunAggregationQueryRequest,
        RunAggregationQueryResponse, RunQueryRequest, RunQueryResponse, StructuredQuery,
        TransactionOptions, UpdateDocumentRequest, Value, Write, WriteRequest, WriteResponse,
        WriteResult, batch_get_documents_request, batch_get_documents_response,
        firestore_server::Firestore, get_document_request, list_collection_ids_request,
        list_documents_request, precondition::ConditionType, run_aggregation_query_request,
        run_query_request, run_query_response, structured_aggregation_query, transaction_options,
        value::ValueType, write::Operation,
    },
    rpc,
};
use prost_types::Timestamp;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming, async_trait, codegen::BoxStream};

use self::{
    aggregation::Aggregations,
    changes::ChangeFeed,
    names::Name,
    query::Query,
    transactions::{Mode, Transactions},
};

const METADATA_ADMIN: &str = "Metadata operations require admin authentication.";
const BATCH_WRITE_ADMIN: &str = "Batch writes require admin authentication.";

/// Marks a request that came in as `google.firestore.v1beta1.Firestore`.
#[derive(Clone, Copy)]
pub(crate) struct V1beta1;

#[derive(Clone)]
pub struct FirestoreService {
    store: Arc<dyn Store>,
    transactions: Arc<Transactions>,
    changes: Arc<ChangeFeed>,
    /// `--database-edition enterprise`: pipelines are allowed (but not implemented yet).
    enterprise: bool,
}

impl FirestoreService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self::with_state(store, Arc::default(), Arc::default())
    }

    pub(crate) fn with_enterprise_edition(mut self, enterprise: bool) -> Self {
        self.enterprise = enterprise;
        self
    }

    pub(crate) fn with_state(
        store: Arc<dyn Store>,
        transactions: Arc<Transactions>,
        changes: Arc<ChangeFeed>,
    ) -> Self {
        Self {
            store,
            transactions,
            changes,
            enterprise: false,
        }
    }

    fn read_time(&self, database: &str, requested: Option<&Timestamp>) -> Result<ReadTime, Status> {
        let latest = self.store.latest_read_time(database);
        let Some(requested) = requested else {
            return Ok(latest);
        };
        let at = ReadTime::from_timestamp(requested);
        if at > latest {
            return Err(Status::invalid_argument(
                "The requested 'read_time' cannot be in the future.",
            ));
        }
        if at < self.store.earliest_read_time(database) {
            return Err(Status::failed_precondition(
                "The requested 'read_time' is too old.",
            ));
        }
        Ok(at)
    }

    /// Runs `writes` as one atomic commit; the first failing write aborts it.
    fn commit_writes(
        &self,
        database: &str,
        writes: &[Write],
    ) -> Result<(ReadTime, Vec<WriteResult>), Status> {
        let mut results = Vec::with_capacity(writes.len());
        let mut failure = None;
        let outcome = self.store.commit(database, &mut |batch| {
            results.clear();
            for write in writes {
                match writes::apply(batch, database, write) {
                    Ok(result) => results.push(result),
                    Err(status) => {
                        failure = Some(status);
                        return Err(StoreError::Aborted(String::new()));
                    }
                }
            }
            Ok(())
        });
        match outcome {
            Ok(commit) => {
                let commit_time = commit.commit_time;
                // Called with the database's commit lock held, so listeners see commits in
                // order (see `changes`).
                self.changes.publish(database, commit);
                Ok((commit_time, results))
            }
            Err(err) => Err(failure.unwrap_or_else(|| store_error(&err))),
        }
    }

    /// Validates `writes`, waits for the locks on their documents and commits them atomically,
    /// in `transaction` if given. `None` for an empty commit, which has no commit time.
    async fn write(
        &self,
        database: &str,
        transaction: Option<&[u8]>,
        writes: &[Write],
    ) -> Result<Option<(ReadTime, Vec<WriteResult>)>, Status> {
        // Invalid writes fail before waiting for any lock, as on the official emulator.
        writes::check_verifies(writes)?;
        let targets = writes
            .iter()
            .map(|write| writes::validate(database, write))
            .collect::<Result<Vec<_>, _>>()?;
        self.transactions
            .commit(database, transaction, &targets, || {
                if writes.is_empty() {
                    return Ok(None);
                }
                self.commit_writes(database, writes).map(Some)
            })
            .await
    }

    /// Opens a transaction with `options` (read-write when absent) and returns its ID.
    fn begin(
        &self,
        database: &str,
        options: Option<&TransactionOptions>,
    ) -> Result<Vec<u8>, Status> {
        let mode = match options.and_then(|o| o.mode.as_ref()) {
            None => Mode::ReadWrite,
            Some(transaction_options::Mode::ReadWrite(read_write)) => {
                if !read_write.retry_transaction.is_empty() {
                    self.transactions
                        .check_retry(database, &read_write.retry_transaction)?;
                }
                Mode::ReadWrite
            }
            Some(transaction_options::Mode::ReadOnly(read_only)) => {
                use transaction_options::read_only::ConsistencySelector;
                let requested = read_only
                    .consistency_selector
                    .as_ref()
                    .map(|ConsistencySelector::ReadTime(ts)| ts);
                Mode::ReadOnly(self.read_time(database, requested)?)
            }
        };
        Ok(self.transactions.begin(database, mode))
    }

    /// The read time of a read in `transaction`, after locking what it reads.
    fn read_in(
        &self,
        database: &str,
        transaction: &[u8],
        documents: &[ResourcePath],
        collection_id: Option<&str>,
    ) -> Result<ReadTime, Status> {
        match self
            .transactions
            .read(database, transaction, documents, collection_id)?
        {
            Mode::ReadOnly(at) => Ok(at),
            Mode::ReadWrite => Ok(self.store.latest_read_time(database)),
        }
    }

    /// The read time of a query (locking `collection_id` in a read-write transaction), and
    /// the ID of the transaction it started, if any.
    fn query_read_time(
        &self,
        database: &str,
        consistency: Consistency<'_>,
        collection_id: Option<&str>,
    ) -> Result<(ReadTime, Option<Vec<u8>>), Status> {
        Ok(match consistency {
            Consistency::Transaction(transaction) => (
                self.read_in(database, transaction, &[], collection_id)?,
                None,
            ),
            Consistency::NewTransaction(options) => {
                let transaction = self.begin(database, Some(options))?;
                let at = self.read_in(database, &transaction, &[], collection_id)?;
                (at, Some(transaction))
            }
            Consistency::ReadTime(ts) => (self.read_time(database, Some(ts))?, None),
            Consistency::Latest => (self.read_time(database, None)?, None),
        })
    }

    fn read_back(
        &self,
        database: &str,
        path: &ResourcePath,
        at: ReadTime,
        mask: Option<&[FieldPath]>,
    ) -> Result<Document, Status> {
        self.store
            .get(database, path, at)
            .map(|doc| to_document(database, &doc, mask))
            .ok_or_else(|| Status::internal("document vanished during the request"))
    }
}

#[async_trait]
impl Firestore for FirestoreService {
    async fn get_document(
        &self,
        request: Request<GetDocumentRequest>,
    ) -> Result<Response<Document>, Status> {
        let (metadata, _, req) = request.into_parts();
        let name = names::document(&req.name)?;
        auth::caller(&metadata)?;
        let at = match &req.consistency_selector {
            Some(get_document_request::ConsistencySelector::Transaction(transaction)) => self
                .read_in(
                    &name.database,
                    transaction,
                    std::slice::from_ref(&name.path),
                    None,
                )?,
            Some(get_document_request::ConsistencySelector::ReadTime(ts)) => {
                self.read_time(&name.database, Some(ts))?
            }
            None => self.read_time(&name.database, None)?,
        };
        let mask = parse_mask(req.mask.as_ref())?;
        match self.store.get(&name.database, &name.path, at) {
            Some(doc) => Ok(Response::new(to_document(
                &name.database,
                &doc,
                mask.as_deref(),
            ))),
            None => Err(Status::not_found(format!(
                "Document ({}) not found.",
                req.name
            ))),
        }
    }

    async fn list_documents(
        &self,
        request: Request<ListDocumentsRequest>,
    ) -> Result<Response<ListDocumentsResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let parent = names::parent(&req.parent)?;
        let caller = auth::caller(&metadata)?;
        names::collection_id(&req.collection_id)?;
        // An empty collection ID lists every collection directly under the parent.
        let every_collection = req.collection_id.is_empty();
        if req.page_size < 0 {
            return Err(Status::invalid_argument("Page size must be nonnegative."));
        }
        if req.show_missing && every_collection {
            return Err(Status::invalid_argument(
                "collection id must be set when show_missing is true",
            ));
        }
        if req.show_missing && !req.order_by.is_empty() {
            return Err(Status::invalid_argument(
                "cannot specify an order when show_missing is true",
            ));
        }
        match req.order_by.as_str() {
            "" | "__name__" | "__name__ asc" => {}
            "__name__ desc" => {
                return Err(Status::failed_precondition(
                    "Firestore does not support descending key scans",
                ));
            }
            _ if every_collection => {
                return Err(Status::invalid_argument(
                    "kind is required for all orders except __key__ ascending",
                ));
            }
            _ => {
                return Err(Status::unimplemented(
                    "ListDocuments order_by other than __name__ is not implemented yet",
                ));
            }
        }
        if req.show_missing && !caller.is_admin() {
            return Err(Status::permission_denied(METADATA_ADMIN));
        }
        let at = match &req.consistency_selector {
            Some(list_documents_request::ConsistencySelector::Transaction(transaction)) => self
                .read_in(
                    &parent.database,
                    transaction,
                    &[],
                    (!every_collection).then_some(req.collection_id.as_str()),
                )?,
            Some(list_documents_request::ConsistencySelector::ReadTime(ts)) => {
                self.read_time(&parent.database, Some(ts))?
            }
            None => self.read_time(&parent.database, None)?,
        };
        let mask = parse_mask(req.mask.as_ref())?;
        // The official emulator ignores the mask when listing missing documents too (the
        // Emulator UI asks for `_none_` and gets every field).
        let mask = if req.show_missing { None } else { mask };
        let collections: Vec<ResourcePath> = if every_collection {
            self.store
                .list_collection_ids(&parent.database, &parent.path, at)
                .into_iter()
                .map(|id| parent.path.child(id))
                .collect()
        } else {
            vec![parent.path.child(req.collection_id.clone())]
        };
        // A page token is the last document listed, in one of the listed collections.
        let after = decode_token(&req.page_token)?
            .map(|p| ResourcePath::parse(&p).ok_or_else(invalid_token))
            .transpose()?;
        let after_collection = after.as_ref().and_then(ResourcePath::parent);
        if let Some(collection) = &after_collection
            && !(if every_collection {
                collection.len() == parent.path.len() + 1 && collection.starts_with(&parent.path)
            } else {
                *collection == collections[0]
            })
        {
            return Err(invalid_token());
        }
        let limit = usize::try_from(req.page_size)
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(usize::MAX);

        let mut documents = Vec::new();
        let mut last = None;
        let mut more = false;
        for collection in &collections {
            if more {
                break;
            }
            let after = match &after_collection {
                Some(c) if c == collection => after.as_ref(),
                // Collections before the token's were listed on earlier pages.
                Some(c)
                    if key::encode_path(collection.segments()) < key::encode_path(c.segments()) =>
                {
                    continue;
                }
                _ => None,
            };
            self.store.list_collection(
                &parent.database,
                collection,
                at,
                ListOptions {
                    after,
                    include_missing: req.show_missing,
                },
                &mut |item| {
                    let (document, path) = match item {
                        ListItem::Document(doc) => (
                            to_document(&parent.database, doc, mask.as_deref()),
                            (*doc.path).clone(),
                        ),
                        ListItem::Missing(path) => (
                            Document {
                                name: Name::document_name(&parent.database, path),
                                ..Document::default()
                            },
                            path.clone(),
                        ),
                    };
                    documents.push(document);
                    last = Some(path);
                    // A full page has a next page, even an empty one, as on the official
                    // emulator.
                    if documents.len() == limit {
                        more = true;
                        return ControlFlow::Break(());
                    }
                    ControlFlow::Continue(())
                },
            );
        }
        Ok(Response::new(ListDocumentsResponse {
            documents,
            next_page_token: if more {
                last.map(|p| encode_token(&p.to_string()))
                    .unwrap_or_default()
            } else {
                String::new()
            },
        }))
    }

    async fn create_document(
        &self,
        request: Request<CreateDocumentRequest>,
    ) -> Result<Response<Document>, Status> {
        let (metadata, _, req) = request.into_parts();
        let parent = names::parent(&req.parent)?;
        names::collection_id(&req.collection_id)?;
        let id = if req.document_id.is_empty() {
            auto_id()
        } else {
            names::document_id(&req.document_id)?;
            req.document_id.clone()
        };
        auth::caller(&metadata)?;
        if req.collection_id.is_empty() {
            return Err(Status::invalid_argument(
                "collectionId is the empty string.",
            ));
        }
        let path = parent.path.child(req.collection_id.clone()).child(id);
        let name = Name::document_name(&parent.database, &path);
        let write = Write {
            operation: Some(Operation::Update(Document {
                name,
                fields: req.document.map(|d| d.fields).unwrap_or_default(),
                ..Document::default()
            })),
            current_document: Some(Precondition {
                condition_type: Some(ConditionType::Exists(false)),
            }),
            ..Write::default()
        };
        let (commit_time, _) = self
            .write(&parent.database, None, &[write])
            .await?
            .expect("one write");
        let mask = parse_mask(req.mask.as_ref())?;
        Ok(Response::new(self.read_back(
            &parent.database,
            &path,
            commit_time,
            mask.as_deref(),
        )?))
    }

    async fn update_document(
        &self,
        request: Request<UpdateDocumentRequest>,
    ) -> Result<Response<Document>, Status> {
        let (metadata, _, req) = request.into_parts();
        // No document is a document without a name, as on the official emulator.
        let document = req.document.unwrap_or_default();
        let name = names::document(&document.name)?;
        auth::caller(&metadata)?;
        let write = Write {
            operation: Some(Operation::Update(document)),
            update_mask: req.update_mask,
            current_document: req.current_document,
            ..Write::default()
        };
        let (commit_time, _) = self
            .write(&name.database, None, &[write])
            .await?
            .expect("one write");
        let mask = parse_mask(req.mask.as_ref())?;
        Ok(Response::new(self.read_back(
            &name.database,
            &name.path,
            commit_time,
            mask.as_deref(),
        )?))
    }

    async fn delete_document(
        &self,
        request: Request<DeleteDocumentRequest>,
    ) -> Result<Response<()>, Status> {
        let (metadata, _, req) = request.into_parts();
        let name = names::document(&req.name)?;
        auth::caller(&metadata)?;
        let write = Write {
            operation: Some(Operation::Delete(req.name)),
            current_document: req.current_document,
            ..Write::default()
        };
        self.write(&name.database, None, &[write]).await?;
        Ok(Response::new(()))
    }

    async fn batch_get_documents(
        &self,
        request: Request<BatchGetDocumentsRequest>,
    ) -> Result<Response<BoxStream<BatchGetDocumentsResponse>>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        auth::caller(&metadata)?;
        let paths = req
            .documents
            .iter()
            .map(|name| {
                let parsed = names::document(name)?;
                if parsed.database == database {
                    Ok(parsed.path)
                } else {
                    Err(Status::invalid_argument(format!(
                        "Document \"{name}\" is not in database \"{database}\"."
                    )))
                }
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let mask = parse_mask(req.mask.as_ref())?;
        let mut responses = Vec::with_capacity(req.documents.len() + 1);
        let at = match &req.consistency_selector {
            Some(batch_get_documents_request::ConsistencySelector::Transaction(transaction)) => {
                self.read_in(&database, transaction, &paths, None)?
            }
            Some(batch_get_documents_request::ConsistencySelector::NewTransaction(options)) => {
                let transaction = self.begin(&database, Some(options))?;
                let at = self.read_in(&database, &transaction, &paths, None)?;
                // The new transaction's ID comes first, in a response of its own.
                responses.push(Ok(BatchGetDocumentsResponse {
                    transaction,
                    read_time: None,
                    result: None,
                }));
                at
            }
            Some(batch_get_documents_request::ConsistencySelector::ReadTime(ts)) => {
                self.read_time(&database, Some(ts))?
            }
            None => self.read_time(&database, None)?,
        };
        // Answers come back in request order, all with the same read time.
        for (name, path) in req.documents.iter().zip(&paths) {
            let result = match self.store.get(&database, path, at) {
                Some(doc) => batch_get_documents_response::Result::Found(to_document(
                    &database,
                    &doc,
                    mask.as_deref(),
                )),
                None => batch_get_documents_response::Result::Missing(name.clone()),
            };
            responses.push(Ok(BatchGetDocumentsResponse {
                transaction: Vec::new(),
                read_time: Some(at.to_timestamp()),
                result: Some(result),
            }));
        }
        Ok(Response::new(Box::pin(tokio_stream::iter(responses))))
    }

    async fn commit(
        &self,
        request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        auth::caller(&metadata)?;
        let transaction = (!req.transaction.is_empty()).then_some(req.transaction.as_slice());
        // The official emulator answers an empty commit with an empty response (no commit time).
        Ok(Response::new(
            match self.write(&database, transaction, &req.writes).await? {
                Some((commit_time, write_results)) => CommitResponse {
                    write_results,
                    commit_time: Some(commit_time.to_timestamp()),
                },
                None => CommitResponse::default(),
            },
        ))
    }

    async fn run_query(
        &self,
        request: Request<RunQueryRequest>,
    ) -> Result<Response<BoxStream<RunQueryResponse>>, Status> {
        let (metadata, _, req) = request.into_parts();
        let parent = names::parent(&req.parent)?;
        auth::caller(&metadata)?;
        let database = parent.database;
        // `explain_options` is ignored, as on the official emulator: the same results, no
        // explain metrics.
        // An absent query is the empty one, as on the official emulator.
        let default = StructuredQuery::default();
        let structured = match &req.query_type {
            Some(run_query_request::QueryType::StructuredQuery(structured)) => structured,
            None => &default,
        };
        let query = Query::parse(parent.path, structured)?;
        let consistency = match &req.consistency_selector {
            Some(run_query_request::ConsistencySelector::Transaction(t)) => {
                Consistency::Transaction(t)
            }
            Some(run_query_request::ConsistencySelector::NewTransaction(options)) => {
                Consistency::NewTransaction(options)
            }
            Some(run_query_request::ConsistencySelector::ReadTime(ts)) => Consistency::ReadTime(ts),
            None => Consistency::Latest,
        };
        let (at, transaction) =
            self.query_read_time(&database, consistency, query.collection_id())?;
        let mut responses = Vec::new();
        if let Some(transaction) = transaction {
            // The new transaction's ID comes first, in a response of its own.
            responses.push(Ok(RunQueryResponse {
                transaction,
                ..RunQueryResponse::default()
            }));
        }
        let results = query.run(self.store.as_ref(), &database, at);
        let read_time = Some(at.to_timestamp());
        let done = Some(run_query_response::ContinuationSelector::Done(true));
        // As on the official emulator: one document per response, `done` on the last one (or
        // on a response of its own when nothing matched), and the number of documents
        // `offset` skipped on every response but the last.
        let skipped = i32::try_from(results.skipped).unwrap_or(i32::MAX);
        let count = results.documents.len();
        if count == 0 {
            responses.push(Ok(RunQueryResponse {
                read_time,
                continuation_selector: done,
                ..RunQueryResponse::default()
            }));
        }
        for (i, doc) in results.documents.iter().enumerate() {
            let last = i + 1 == count;
            let document = match (query.distance_field(), results.distances.get(i)) {
                // The distance goes in before the projection, which can leave it out.
                (Some(field), Some(&distance)) => {
                    let mut fields = doc.fields();
                    fields.insert(
                        field.to_owned(),
                        Value {
                            value_type: Some(ValueType::DoubleValue(distance)),
                        },
                    );
                    document_with(&database, doc, fields, query.projection())
                }
                _ => to_document(&database, doc, query.projection()),
            };
            responses.push(Ok(RunQueryResponse {
                document: Some(document),
                read_time,
                skipped_results: if last { 0 } else { skipped },
                continuation_selector: if last { done } else { None },
                ..RunQueryResponse::default()
            }));
        }
        Ok(Response::new(Box::pin(tokio_stream::iter(responses))))
    }

    async fn run_aggregation_query(
        &self,
        request: Request<RunAggregationQueryRequest>,
    ) -> Result<Response<BoxStream<RunAggregationQueryResponse>>, Status> {
        let (metadata, _, req) = request.into_parts();
        let parent = names::parent(&req.parent)?;
        auth::caller(&metadata)?;
        let database = parent.database;
        // `explain_options` is ignored, as on the official emulator: the same results, no
        // explain metrics.
        let default = StructuredQuery::default();
        let (structured, aggregations) = match &req.query_type {
            Some(run_aggregation_query_request::QueryType::StructuredAggregationQuery(q)) => (
                match &q.query_type {
                    Some(structured_aggregation_query::QueryType::StructuredQuery(s)) => s,
                    None => &default,
                },
                q.aggregations.as_slice(),
            ),
            None => (&default, &[][..]),
        };
        let mut query = Query::parse(parent.path, structured)?;
        let aggregations = Aggregations::parse(aggregations)?;
        query.require(aggregations.fields());
        let consistency = match &req.consistency_selector {
            Some(run_aggregation_query_request::ConsistencySelector::Transaction(t)) => {
                Consistency::Transaction(t)
            }
            Some(run_aggregation_query_request::ConsistencySelector::NewTransaction(options)) => {
                Consistency::NewTransaction(options)
            }
            Some(run_aggregation_query_request::ConsistencySelector::ReadTime(ts)) => {
                Consistency::ReadTime(ts)
            }
            None => Consistency::Latest,
        };
        let (at, transaction) =
            self.query_read_time(&database, consistency, query.collection_id())?;
        let mut responses = Vec::new();
        if let Some(transaction) = transaction {
            // As for RunQuery: the new transaction's ID first, on its own.
            responses.push(Ok(RunAggregationQueryResponse {
                transaction,
                ..RunAggregationQueryResponse::default()
            }));
        }
        let results = query.run(self.store.as_ref(), &database, at);
        // The official emulator also sets `done: true`, a field the published protos lack.
        responses.push(Ok(RunAggregationQueryResponse {
            result: Some(AggregationResult {
                aggregate_fields: aggregations.compute(&results.documents),
            }),
            read_time: Some(at.to_timestamp()),
            ..RunAggregationQueryResponse::default()
        }));
        Ok(Response::new(Box::pin(tokio_stream::iter(responses))))
    }

    async fn write(
        &self,
        request: Request<Streaming<WriteRequest>>,
    ) -> Result<Response<BoxStream<WriteResponse>>, Status> {
        auth::caller(request.metadata())?;
        let mut requests = request.into_inner();
        let (responses, receiver) = mpsc::channel(16);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(status) = service.serve_write_stream(&mut requests, &responses).await {
                let _ = responses.send(Err(status)).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn listen(
        &self,
        request: Request<Streaming<ListenRequest>>,
    ) -> Result<Response<BoxStream<ListenResponse>>, Status> {
        auth::caller(request.metadata())?;
        let mut requests = request.into_inner();
        let (responses, receiver) = mpsc::channel(256);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(status) = service.serve_listen(&mut requests, &responses).await {
                let _ = responses.send(Err(status)).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn partition_query(
        &self,
        request: Request<PartitionQueryRequest>,
    ) -> Result<Response<PartitionQueryResponse>, Status> {
        // The official emulator's answer (#26), naming the service called.
        let version = if request.extensions().get::<V1beta1>().is_some() {
            "v1beta1"
        } else {
            "v1"
        };
        Err(Status::unimplemented(format!(
            "Method google.firestore.{version}.Firestore/PartitionQuery is unimplemented"
        )))
    }

    async fn execute_pipeline(
        &self,
        request: Request<ExecutePipelineRequest>,
    ) -> Result<Response<BoxStream<ExecutePipelineResponse>>, Status> {
        let (metadata, _, req) = request.into_parts();
        names::database(&req.database)?;
        // A standard-edition database refuses pipelines before reading the header, as on the
        // official emulator.
        if !self.enterprise {
            return Err(Status::invalid_argument(
                "ExecutePipeline requires the Database Edition to be `enterprise`.",
            ));
        }
        auth::caller(&metadata)?;
        Err(Status::unimplemented(
            "ExecutePipeline is not implemented yet (https://github.com/hidane-dev/hidane/issues/80)",
        ))
    }

    async fn begin_transaction(
        &self,
        request: Request<BeginTransactionRequest>,
    ) -> Result<Response<BeginTransactionResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        auth::caller(&metadata)?;
        Ok(Response::new(BeginTransactionResponse {
            transaction: self.begin(&database, req.options.as_ref())?,
        }))
    }

    async fn rollback(&self, request: Request<RollbackRequest>) -> Result<Response<()>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        auth::caller(&metadata)?;
        self.transactions.rollback(&database, &req.transaction)?;
        Ok(Response::new(()))
    }

    async fn batch_write(
        &self,
        request: Request<BatchWriteRequest>,
    ) -> Result<Response<BatchWriteResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        let caller = auth::caller(&metadata)?;
        let mut seen = HashSet::new();
        if !req
            .writes
            .iter()
            .filter_map(writes::target)
            .all(|name| seen.insert(name))
        {
            return Err(Status::invalid_argument(
                "the same document cannot be written more than once in a single request",
            ));
        }
        // An invalid write fails the whole request, as on the official emulator.
        for write in &req.writes {
            writes::validate(&database, write)?;
        }
        if !caller.is_admin() {
            return Err(Status::permission_denied(BATCH_WRITE_ADMIN));
        }
        // Otherwise writes are independent: each one waits for its own locks and commits on its
        // own (the official emulator gives each a separate commit time), and one that fails its
        // precondition or its lock wait is reported while the others still apply.
        let mut write_results = Vec::with_capacity(req.writes.len());
        let mut status = Vec::with_capacity(req.writes.len());
        for write in &req.writes {
            match self
                .write(&database, None, std::slice::from_ref(write))
                .await
            {
                Ok(outcome) => {
                    let (_, mut results) = outcome.expect("one write");
                    write_results.push(results.pop().unwrap_or_default());
                    status.push(rpc::Status::default());
                }
                Err(err) => {
                    write_results.push(WriteResult::default());
                    status.push(rpc::Status {
                        code: err.code() as i32,
                        message: err.message().to_owned(),
                        details: Vec::new(),
                    });
                }
            }
        }
        Ok(Response::new(BatchWriteResponse {
            write_results,
            status,
        }))
    }

    async fn list_collection_ids(
        &self,
        request: Request<ListCollectionIdsRequest>,
    ) -> Result<Response<ListCollectionIdsResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let parent = names::parent(&req.parent)?;
        let caller = auth::caller(&metadata)?;
        if req.page_size < 0 {
            return Err(Status::invalid_argument(
                "page_size must be greater than or equal to zero.",
            ));
        }
        if !caller.is_admin() {
            return Err(Status::permission_denied(METADATA_ADMIN));
        }
        let at = match &req.consistency_selector {
            Some(list_collection_ids_request::ConsistencySelector::ReadTime(ts)) => {
                self.read_time(&parent.database, Some(ts))?
            }
            None => self.read_time(&parent.database, None)?,
        };
        let after = decode_token(&req.page_token)?;
        let limit = usize::try_from(req.page_size)
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(usize::MAX);
        let mut ids: Vec<String> = self
            .store
            .list_collection_ids(&parent.database, &parent.path, at)
            .into_iter()
            .filter(|id| after.as_ref().is_none_or(|after| id > after))
            .collect();
        // A full page has a next page, even an empty one, as on the official emulator.
        let more = ids.len() >= limit;
        ids.truncate(limit);
        let next_page_token = if more {
            ids.last().map(|id| encode_token(id)).unwrap_or_default()
        } else {
            String::new()
        };
        Ok(Response::new(ListCollectionIdsResponse {
            collection_ids: ids,
            next_page_token,
        }))
    }
}

/// How a query reads, whatever its request type.
enum Consistency<'a> {
    Transaction(&'a [u8]),
    NewTransaction(&'a TransactionOptions),
    ReadTime(&'a Timestamp),
    Latest,
}

fn to_document(database: &str, doc: &StoredDocument, mask: Option<&[FieldPath]>) -> Document {
    document_with(database, doc, doc.fields(), mask)
}

/// `doc` with `fields` in place of its own.
fn document_with(
    database: &str,
    doc: &StoredDocument,
    fields: Fields,
    mask: Option<&[FieldPath]>,
) -> Document {
    Document {
        name: Name::document_name(database, &doc.path),
        fields: match mask {
            Some(mask) => project(&fields, mask),
            None => fields,
        },
        create_time: Some(doc.create_time),
        update_time: Some(doc.update_time),
    }
}

fn parse_mask(mask: Option<&DocumentMask>) -> Result<Option<Vec<FieldPath>>, Status> {
    mask.map(|m| {
        m.field_paths
            .iter()
            .map(|p| FieldPath::parse(p).map_err(Status::invalid_argument))
            .collect()
    })
    .transpose()
}

fn store_error(err: &StoreError) -> Status {
    match err {
        StoreError::InvalidArgument(m) => Status::invalid_argument(m.clone()),
        StoreError::NotFound(m) => Status::not_found(m.clone()),
        StoreError::AlreadyExists(m) => Status::already_exists(m.clone()),
        StoreError::FailedPrecondition(m) => Status::failed_precondition(m.clone()),
        StoreError::Aborted(m) => Status::aborted(m.clone()),
    }
}

fn encode_token(position: &str) -> String {
    URL_SAFE_NO_PAD.encode(position)
}

fn decode_token(token: &str) -> Result<Option<String>, Status> {
    if token.is_empty() {
        return Ok(None);
    }
    URL_SAFE_NO_PAD
        .decode(token)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(Some)
        .ok_or_else(invalid_token)
}

fn invalid_token() -> Status {
    Status::invalid_argument("Invalid page token.")
}

/// A 20-character alphanumeric ID, like the ones Firestore generates.
fn auto_id() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let random = std::collections::hash_map::RandomState::new();
    let mut id = String::with_capacity(20);
    let mut bits = 0u64;
    for i in 0..20 {
        if i % 10 == 0 {
            let mut hasher = random.build_hasher();
            hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
            bits = hasher.finish();
        }
        id.push(char::from(
            ALPHABET[usize::try_from(bits % 62).unwrap_or(0)],
        ));
        bits /= 62;
    }
    id
}
