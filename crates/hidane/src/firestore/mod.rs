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
mod export;
mod guard;
pub(crate) mod index_usage;
mod listen;
pub(crate) mod names;
mod pipeline;
mod property_path;
mod query;
pub(crate) mod seed;
pub(crate) mod transactions;
mod validate;
mod write_stream;
mod writes;

use std::{
    collections::{HashMap, HashSet},
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
    store::{Change, ListItem, ListOptions, ReadTime, Store, StoreError, StoredDocument},
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
        run_query_request, run_query_response, structured_aggregation_query, structured_query,
        transaction_options, value::ValueType, write::Operation,
    },
    rpc,
};
use prost_types::Timestamp;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request, Response, Status, Streaming, async_trait, codegen::BoxStream, metadata::MetadataMap,
};

use crate::single_project::{self, SingleProject};

use self::{
    aggregation::Aggregations,
    changes::ChangeFeed,
    names::Name,
    query::Query,
    transactions::{Mode, Transactions},
    writes::Verifies,
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
    /// `--seed_from_export`, forgotten on `POST /reset`.
    seeder: Option<Arc<seed::Seeder>>,
    /// The Security Rules of each database (`:securityRules`, `--rules`).
    rules: Option<Arc<crate::rules::Rules>>,
    /// The Functions emulator's Firestore triggers, which commits send events to.
    triggers: Option<Arc<crate::triggers::Triggers>>,
    /// `--single_project_mode`: what a request for another project gets.
    single_project: Option<Arc<SingleProject>>,
    /// The composite indexes queries needed, for `:indexUsage`.
    index_usage: Option<Arc<index_usage::IndexUsage>>,
}

impl FirestoreService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self::with_state(store, Arc::default(), Arc::default())
    }

    pub(crate) fn with_enterprise_edition(mut self, enterprise: bool) -> Self {
        self.enterprise = enterprise;
        self
    }

    pub(crate) fn with_seeder(mut self, seeder: Option<Arc<seed::Seeder>>) -> Self {
        self.seeder = seeder;
        self
    }

    pub(crate) fn with_triggers(mut self, triggers: Arc<crate::triggers::Triggers>) -> Self {
        self.triggers = Some(triggers);
        self
    }

    pub(crate) fn with_single_project(mut self, single: Option<Arc<SingleProject>>) -> Self {
        self.single_project = single;
        self
    }

    pub(crate) fn with_index_usage(mut self, usage: Arc<index_usage::IndexUsage>) -> Self {
        self.index_usage = Some(usage);
        self
    }

    /// Counts the composite indexes a query that ran on `database` needed, for `:indexUsage`.
    /// On an enterprise database only a listener's query counts, which is checked as a standard
    /// one, as on the official emulator (`tests/fixtures/index_usage_enterprise.json`).
    fn record_indexes(&self, database: &str, query: &Query) {
        if let Some(usage) = &self.index_usage
            && !query.is_enterprise()
        {
            usage.record(database, query.needed_indexes());
        }
    }

    /// The caller of a request addressed to `database` (a stream's: to its
    /// `google-cloud-resource-prefix` header alone), between the checks of
    /// `--single_project_mode` (`crate::single_project`).
    fn caller(
        &self,
        metadata: &MetadataMap,
        database: Option<&str>,
    ) -> Result<auth::Caller, Status> {
        match (
            &self.single_project,
            single_project::requested(metadata, database),
        ) {
            (Some(single), Some(project)) => single.admit(&project, || auth::caller(metadata)),
            _ => auth::caller(metadata),
        }
    }

    /// What `read` reads (the `Authorization` header, or nothing) between the checks of
    /// `--single_project_mode` for a request addressed to `database`, outside gRPC.
    pub(crate) fn admit<T>(
        &self,
        database: &str,
        read: impl FnOnce() -> Result<T, Status>,
    ) -> Result<T, Status> {
        match &self.single_project {
            Some(single) => single.admit(single_project::project(database), read),
            None => read(),
        }
    }

    pub(crate) fn single_project(&self) -> Option<&SingleProject> {
        self.single_project.as_deref()
    }

    /// Sends the events of a commit to `database` at `at` (`changes`, in the order `writes`
    /// name their documents) to the triggers that watch them. Callers hold the database's
    /// commit lock.
    fn send_events(&self, database: &str, at: ReadTime, changes: &[Change], writes: &[Write]) {
        let Some(triggers) = self.triggers.as_ref().filter(|t| t.watches(database)) else {
            return;
        };
        let mut order: HashMap<&str, usize> = HashMap::new();
        for (i, write) in writes.iter().enumerate() {
            if let Some(name) = writes::target(write) {
                order.entry(name).or_insert(i);
            }
        }
        let mut ordered: Vec<(usize, &Change)> = changes
            .iter()
            .map(|change| {
                let name = Name::document_name(database, &change.path);
                (
                    order.get(name.as_str()).copied().unwrap_or(usize::MAX),
                    change,
                )
            })
            .collect();
        ordered.sort_by_key(|(i, _)| *i);
        let ordered: Vec<&Change> = ordered.into_iter().map(|(_, change)| change).collect();
        triggers.committed(database, at, &ordered);
    }

    pub(crate) fn with_rules(mut self, rules: Arc<crate::rules::Rules>) -> Self {
        self.rules = Some(rules);
        self
    }

    /// Security Rules on a query: a `list` of its collection.
    /// Security Rules for a query as read ([`Query::read`]): they normalize its filter first,
    /// refusing too many disjunctions before they decide, and come before the rest of the
    /// query is checked (`tools/oracle/rules_list.py`, #262).
    fn check_list(
        &self,
        caller: &auth::Caller,
        database: &str,
        query: &query::Unchecked<'_>,
    ) -> Result<(), Status> {
        let Some(guard) = self.guard(caller, database) else {
            return Ok(());
        };
        query.check_disjunctions()?;
        let (collection, view) = query.rules_view();
        let at = self.store.latest_read_time(database);
        guard
            .list(&collection, view, &|path| {
                self.store.get(database, path, at)
            })
            .map_err(guard::denied)
    }

    /// The checks of a request by `caller` on `database`, if any apply (`guard`).
    fn guard(&self, caller: &auth::Caller, database: &str) -> Option<guard::Guard> {
        if matches!(caller, auth::Caller::Admin) {
            return None;
        }
        let rules = self.rules.as_ref()?;
        guard::Guard::new(
            rules.checked(database)?,
            Arc::clone(rules),
            caller,
            database,
        )
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
            seeder: None,
            rules: None,
            triggers: None,
            single_project: None,
            index_usage: None,
        }
    }

    fn read_time(&self, database: &str, requested: Option<&Timestamp>) -> Result<ReadTime, Status> {
        let latest = self.store.latest_read_time(database);
        let Some(requested) = requested else {
            return Ok(latest);
        };
        writes::base_version(requested)?;
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

    /// Runs `writes` as one atomic commit; the first failing write aborts it. With `guard`,
    /// Security Rules check every write once all are applied (`guard`), the writes to one
    /// document as one ([`guard::CheckedWrite::merge`]); a denied one aborts it.
    /// The rules come before the preconditions: a write whose precondition fails is checked as
    /// if it had none, with the writes after it, and its failure is reported only if the rules
    /// allow them all (`tools/oracle/rules_verify.py`).
    fn commit_writes(
        &self,
        database: &str,
        writes: &[Write],
        guard: Option<&guard::Guard>,
    ) -> Result<(ReadTime, Vec<WriteResult>), Status> {
        let mut results = Vec::with_capacity(writes.len());
        let mut failure = None;
        let outcome = self.store.commit(database, &mut |batch| {
            results.clear();
            let mut checked: Vec<guard::CheckedWrite> = Vec::new();
            // Where `checked` holds the writes to each document, but verifies.
            let mut merged: HashMap<ResourcePath, usize> = HashMap::new();
            let mut record = |view: guard::CheckedWrite| {
                if view.method == hidane_rules::eval::Method::Get {
                    checked.push(view);
                } else if let Some(&at) = merged.get(&view.path) {
                    checked[at].merge(view);
                } else {
                    merged.insert(view.path.clone(), checked.len());
                    checked.push(view);
                }
            };
            let mut before = guard::Before {
                touched: HashMap::new(),
            };
            // The first precondition that failed, if the rules check the writes.
            let mut precondition = None;
            let mut touched = writes::Touched::default();
            for write in writes {
                let first = touched.record(&*batch, write);
                let view = guard.and_then(|_| writes::rules_method(&*batch, write));
                if let Some(view) = &view {
                    before
                        .touched
                        .entry(view.path.clone())
                        .or_insert_with(|| view.before.clone());
                }
                match writes::apply(batch, write, first) {
                    Ok(result) => {
                        results.push(result);
                        if let Some(view) = view {
                            record(view);
                        }
                    }
                    // The rules come before the precondition: check the write as if it had
                    // none (a verify has nothing else to apply), and go on with the others.
                    Err(status) if view.is_some() && writes::is_precondition(&status) => {
                        let view = view.expect("checked");
                        let applied = view.method == hidane_rules::eval::Method::Get || {
                            let mut unconditional = write.clone();
                            unconditional.current_document = None;
                            writes::apply(batch, &unconditional, first).is_ok()
                        };
                        if applied {
                            record(view);
                        }
                        precondition.get_or_insert(status);
                    }
                    // Any other failure ends the commit, after the rules of the writes before
                    // it if a precondition failed already.
                    Err(status) => {
                        if precondition.is_none() {
                            failure = Some(status);
                            return Err(StoreError::Aborted(String::new()));
                        }
                        break;
                    }
                }
            }
            // What the writes to each document leave, once all are applied.
            for view in &mut checked {
                view.after = batch.get(&view.path);
            }
            if let Some(guard) = guard
                && let Err(message) =
                    check_writes(guard, &checked, writes.len() > 1, &before, batch)
            {
                failure = Some(guard::denied(message));
                return Err(StoreError::Aborted(String::new()));
            }
            if let Some(status) = precondition {
                failure = Some(status);
                return Err(StoreError::Aborted(String::new()));
            }
            touched.settle(batch, writes, &mut results);
            Ok(())
        });
        match outcome {
            Ok(commit) => {
                let commit_time = commit.commit_time;
                // Called with the database's commit lock held, so listeners see commits in
                // order (see `changes`), and so do triggers.
                self.send_events(database, commit_time, &commit.changes, writes);
                self.changes.publish(database, commit);
                Ok((commit_time, results))
            }
            Err(err) => Err(failure.unwrap_or_else(|| store_error(&err))),
        }
    }

    /// Checks `writes`, waits for the locks on their documents and commits them atomically,
    /// in `transaction` if given. `None` for an empty commit, which has no commit time.
    async fn write(
        &self,
        database: &str,
        transaction: Option<&[u8]>,
        writes: &[Write],
        guard: Option<&guard::Guard>,
        verifies: writes::Verifies,
    ) -> Result<Option<(ReadTime, Vec<WriteResult>)>, Status> {
        // The transaction comes first, then invalid writes, before waiting for any lock, as on
        // the official emulator.
        if let Some(transaction) = transaction {
            self.transactions
                .check_commit(database, transaction, !writes.is_empty())?;
        }
        let targets = writes::check(database, writes, verifies)?;
        self.transactions
            .commit(database, transaction, &targets, || {
                if writes.is_empty() {
                    return Ok(None);
                }
                self.commit_writes(database, writes, guard).map(Some)
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

/// Checks the writes of a commit: `get()` reads each document as it was before the commit,
/// `getAfter()` as the batch leaves it.
fn check_writes(
    guard: &guard::Guard,
    checked: &[guard::CheckedWrite],
    several: bool,
    before: &guard::Before,
    batch: &dyn hidane_core::store::WriteBatch,
) -> Result<(), String> {
    // `request.time` is the time `REQUEST_TIME` writes, to the millisecond.
    let time = guard::timestamp(&hidane_core::transform::request_time(
        &batch.commit_time().to_timestamp(),
    ));
    guard.writes(
        checked,
        several,
        time,
        &|path| before.get(path, |p| batch.get(p)),
        &|path| batch.get(path),
    )
}

#[async_trait]
impl Firestore for FirestoreService {
    async fn get_document(
        &self,
        request: Request<GetDocumentRequest>,
    ) -> Result<Response<Document>, Status> {
        let (metadata, _, req) = request.into_parts();
        let name = names::document(&req.name)?;
        let caller = self.caller(&metadata, Some(&name.database))?;
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
        let stored = self.store.get(&name.database, &name.path, at);
        let in_transaction = matches!(
            req.consistency_selector,
            Some(get_document_request::ConsistencySelector::Transaction(_))
        );
        if let Some(guard) = self.guard(&caller, &name.database) {
            guard
                .in_transaction(in_transaction)
                .get(&name.path, stored.as_deref(), &|path| {
                    self.store.get(&name.database, path, at)
                })
                .map_err(guard::denied)?;
        }
        match stored {
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
        let caller = self.caller(&metadata, Some(&parent.database))?;
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
        // The order is a query's (`tools/oracle/rest.py`, #262): its clauses, then the query's
        // checks of its orders. Ordered by anything but the name, the listing is that query's.
        let orders = list_orders(&req.order_by)?;
        let ordered = Query::parse(
            &parent.database,
            parent.path.clone(),
            &StructuredQuery {
                from: if every_collection {
                    Vec::new()
                } else {
                    vec![structured_query::CollectionSelector {
                        collection_id: req.collection_id.clone(),
                        all_descendants: false,
                    }]
                },
                order_by: orders,
                ..StructuredQuery::default()
            },
        )?;
        let ordered = (!ordered.orders_by_name()).then_some(ordered);
        if req.show_missing && !caller.is_admin() {
            return Err(Status::permission_denied(METADATA_ADMIN));
        }
        // A list of the collection, with `pageSize` as its limit.
        if let Some(guard) = self.guard(&caller, &parent.database) {
            let collection = parent.path.child(req.collection_id.clone());
            // Without a page size, rules see a limit of 30 (`tools/oracle/rules_list.py`); an
            // order (by name) shows, and the mask as `request.fields`
            // (`tools/oracle/rules_request_fields.py`).
            let view = hidane_rules::eval::Query {
                limit: Some(if req.page_size > 0 {
                    i64::from(req.page_size)
                } else {
                    30
                }),
                offset: Some(0),
                order_by: if req.order_by.is_empty() {
                    Vec::new()
                } else {
                    vec![("__name__".to_owned(), "ASC".to_owned())]
                },
                fields: parse_mask(req.mask.as_ref())
                    .ok()
                    .flatten()
                    .map(|mask| guard::rules_paths(&mask)),
                ..hidane_rules::eval::Query::default()
            };
            let at = self.store.latest_read_time(&parent.database);
            guard
                .list(&collection, view, &|path| {
                    self.store.get(&parent.database, path, at)
                })
                .map_err(guard::denied)?;
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

        if let Some(query) = ordered {
            // A page token's document that is gone ends the listing.
            let after = after.map(|path| self.store.get(&parent.database, &path, at));
            let found = match &after {
                Some(None) => Vec::new(),
                Some(Some(doc)) => query.run_after(&*self.store, &parent.database, at, Some(doc)),
                None => query.run_after(&*self.store, &parent.database, at, None),
            };
            // A full page has a next page, even an empty one, ordered or not (#313).
            let more = found.len() >= limit;
            let page: Vec<_> = found.into_iter().take(limit).collect();
            return Ok(Response::new(ListDocumentsResponse {
                next_page_token: match page.last() {
                    Some(last) if more => encode_token(&last.path.to_string()),
                    _ => String::new(),
                },
                documents: page
                    .iter()
                    .map(|doc| to_document(&parent.database, doc, mask.as_deref()))
                    .collect(),
            }));
        }

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
        let caller = self.caller(&metadata, Some(&parent.database))?;
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
        let guard = self
            .guard(&caller, &parent.database)
            .map(|guard| guard.auto_id(req.document_id.is_empty()));
        let mask = read_mask(&parent.database, &write, req.mask.as_ref())?;
        let (commit_time, _) = self
            .write(
                &parent.database,
                None,
                &[write],
                guard.as_ref(),
                Verifies::Checked,
            )
            .await?
            .expect("one write");
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
        let caller = self.caller(&metadata, Some(&name.database))?;
        let write = Write {
            operation: Some(Operation::Update(document)),
            update_mask: req.update_mask,
            current_document: req.current_document,
            ..Write::default()
        };
        let guard = self.guard(&caller, &name.database);
        let mask = read_mask(&name.database, &write, req.mask.as_ref())?;
        let (commit_time, _) = self
            .write(
                &name.database,
                None,
                &[write],
                guard.as_ref(),
                Verifies::Checked,
            )
            .await?
            .expect("one write");
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
        let caller = self.caller(&metadata, Some(&name.database))?;
        let write = Write {
            operation: Some(Operation::Delete(req.name)),
            current_document: req.current_document,
            ..Write::default()
        };
        let guard = self.guard(&caller, &name.database);
        self.write(
            &name.database,
            None,
            &[write],
            guard.as_ref(),
            Verifies::Checked,
        )
        .await?;
        Ok(Response::new(()))
    }

    async fn batch_get_documents(
        &self,
        request: Request<BatchGetDocumentsRequest>,
    ) -> Result<Response<BoxStream<BatchGetDocumentsResponse>>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        let caller = self.caller(&metadata, Some(&database))?;
        // A document named more than once is read and answered once, where it is first named,
        // as on the official emulator (#262).
        let mut documents: Vec<(&String, ResourcePath)> = Vec::with_capacity(req.documents.len());
        for name in &req.documents {
            let parsed = names::document(name)?;
            if parsed.database != database {
                return Err(Status::invalid_argument(format!(
                    "Document \"{name}\" is not in database \"{database}\"."
                )));
            }
            if !documents.iter().any(|(_, path)| *path == parsed.path) {
                documents.push((name, parsed.path));
            }
        }
        let paths: Vec<ResourcePath> = documents.iter().map(|(_, path)| path.clone()).collect();
        let mask = parse_mask(req.mask.as_ref())?;
        let mut responses = Vec::with_capacity(documents.len() + 1);
        let mut begun = None;
        let at = match &req.consistency_selector {
            Some(batch_get_documents_request::ConsistencySelector::Transaction(transaction)) => {
                self.read_in(&database, transaction, &paths, None)?
            }
            Some(batch_get_documents_request::ConsistencySelector::NewTransaction(options)) => {
                let transaction = self.begin(&database, Some(options))?;
                let at = self.read_in(&database, &transaction, &paths, None)?;
                begun = Some(transaction.clone());
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
        // One denied document denies the whole call, missing documents included. The documents
        // are checked in request order and the first denied one decides, its reasons alone
        // (`docs/parity-exceptions.md` D019).
        let in_transaction = matches!(
            req.consistency_selector,
            Some(batch_get_documents_request::ConsistencySelector::Transaction(_))
        );
        if let Some(guard) = self
            .guard(&caller, &database)
            .map(|guard| guard.in_transaction(in_transaction))
        {
            let denial = paths.iter().find_map(|path| {
                let stored = self.store.get(&database, path, at);
                guard
                    .get(path, stored.as_deref(), &|p| {
                        self.store.get(&database, p, at)
                    })
                    .err()
            });
            if let Some(denial) = denial {
                // The transaction the call began ends with it: its ID is never sent, and the
                // official emulator leaves nothing locked (`tools/oracle/rules_expressions.py`).
                if let Some(transaction) = begun {
                    self.transactions.rollback(&database, &transaction)?;
                }
                return Err(guard::denied(denial));
            }
        }
        // Answers come back in request order, all with the same read time.
        for (name, path) in &documents {
            let result = match self.store.get(&database, path, at) {
                Some(doc) => batch_get_documents_response::Result::Found(to_document(
                    &database,
                    &doc,
                    mask.as_deref(),
                )),
                None => batch_get_documents_response::Result::Missing((*name).clone()),
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
        let caller = self.caller(&metadata, Some(&database))?;
        let transaction = (!req.transaction.is_empty()).then_some(req.transaction.as_slice());
        // A pipeline's `update` or `delete` in the transaction comes first.
        let pending = transaction
            .map(|t| self.transactions.take_pending(&database, t))
            .unwrap_or_default();
        let writes = if pending.is_empty() {
            req.writes
        } else {
            pending.into_iter().chain(req.writes).collect()
        };
        // The official emulator answers an empty commit with an empty response (no commit time).
        Ok(Response::new(
            match self
                .write(
                    &database,
                    transaction,
                    &writes,
                    self.guard(&caller, &database).as_ref(),
                    Verifies::Checked,
                )
                .await?
            {
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
        let caller = self.caller(&metadata, Some(&parent.database))?;
        let database = parent.database;
        // `explain_options` is ignored, as on the official emulator: the same results, no
        // explain metrics.
        // The operators of the query's filter come first, then the transaction the request
        // reads in: a malformed or ended one is reported before anything else about the query
        // (`tools/oracle/transactions.py`, `tools/oracle/queries.py`, #262).
        if let Some(run_query_request::QueryType::StructuredQuery(structured)) = &req.query_type
            && !self.enterprise
        {
            query::check_operators(structured)?;
        }
        if let Some(run_query_request::ConsistencySelector::Transaction(t)) =
            &req.consistency_selector
        {
            self.read_in(&database, t, &[], None)?;
        }
        let Some(run_query_request::QueryType::StructuredQuery(structured)) = &req.query_type
        else {
            return Err(Status::invalid_argument(
                "only structured queries are supported",
            ));
        };
        // Security Rules, then the read time, come before the rest of the query is checked
        // (`tools/oracle/rules_list.py`, `tools/oracle/queries.py`, #262). A transaction locks
        // after it: one the checks refuse locks nothing (D085).
        let read = Query::read(&database, parent.path, structured, self.enterprise, |_| {
            Ok(Vec::new())
        })?;
        self.check_list(&caller, &database, &read)?;
        if let Some(run_query_request::ConsistencySelector::ReadTime(ts)) =
            &req.consistency_selector
        {
            self.read_time(&database, Some(ts))?;
        }
        let query = if self.enterprise {
            read.check_enterprise()?
        } else {
            read.check()?
        };
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
        let results = query.try_run(self.store.as_ref(), &database, at)?;
        self.record_indexes(&database, &query);
        let read_time = Some(at.to_timestamp());
        let done = Some(run_query_response::ContinuationSelector::Done(true));
        // As on the official emulator: one document per response, `done` on the last one (or
        // on a response of its own when nothing matched), and the number of documents
        // `offset` skipped on every response but the last. An enterprise database sends
        // neither on a document (`tests/fixtures/queries_enterprise.json`).
        let enterprise = query.is_enterprise();
        let skipped = if enterprise {
            0
        } else {
            i32::try_from(results.skipped).unwrap_or(i32::MAX)
        };
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
                _ if enterprise => {
                    let fields = doc.fields();
                    let projected = query
                        .projection()
                        .map(|paths| hidane_core::field_path::project_keeping_maps(&fields, paths));
                    document_with(&database, doc, projected.unwrap_or(fields), None)
                }
                _ => to_document(&database, doc, query.projection()),
            };
            responses.push(Ok(RunQueryResponse {
                document: Some(document),
                read_time,
                skipped_results: if last { 0 } else { skipped },
                continuation_selector: if last && !enterprise { done } else { None },
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
        let caller = self.caller(&metadata, Some(&parent.database))?;
        let database = parent.database;
        // `explain_options` is ignored, as on the official emulator: the same results, no
        // explain metrics.
        // As for RunQuery, the operators of the query's filter, then the transaction. Then the
        // aggregations are read, the query's parts read, the aggregations checked as a list,
        // and the rest of the query checked (`tools/oracle/aggregations.py`, #262). An
        // aggregation query without a query aggregates the empty one.
        let default = StructuredQuery::default();
        let structured = match &req.query_type {
            Some(run_aggregation_query_request::QueryType::StructuredAggregationQuery(q)) => {
                match &q.query_type {
                    Some(structured_aggregation_query::QueryType::StructuredQuery(s)) => s,
                    None => &default,
                }
            }
            None => &default,
        };
        if !self.enterprise {
            query::check_operators(structured)?;
        }
        if let Some(run_aggregation_query_request::ConsistencySelector::Transaction(t)) =
            &req.consistency_selector
        {
            self.read_in(&database, t, &[], None)?;
        }
        let Some(run_aggregation_query_request::QueryType::StructuredAggregationQuery(q)) =
            &req.query_type
        else {
            return Err(Status::invalid_argument(
                "Only structured aggregation queries are supported.",
            ));
        };
        let listed = Aggregations::read(&q.aggregations)?;
        let mut checked = None;
        let enterprise = self.enterprise;
        let read = Query::read(
            &database,
            parent.path,
            structured,
            enterprise,
            |projection| {
                // An enterprise database leaves no document out for the fields aggregated (#338).
                let (aggregations, fields) = if enterprise {
                    (listed.check_enterprise()?, Vec::new())
                } else {
                    let aggregations = listed.check(projection)?;
                    let fields = aggregations.fields().collect();
                    (aggregations, fields)
                };
                checked = Some(aggregations);
                Ok(fields)
            },
        )?;
        let aggregations = checked.expect("checked while reading the query");
        // As for RunQuery: Security Rules and the read time, the rest, then the lock.
        self.check_list(&caller, &database, &read)?;
        if let Some(run_aggregation_query_request::ConsistencySelector::ReadTime(ts)) =
            &req.consistency_selector
        {
            self.read_time(&database, Some(ts))?;
        }
        let query = if enterprise {
            read.check_enterprise()?
        } else {
            let mut query = read.check()?;
            query.require(aggregations.fields());
            query
        };
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
        let results = query.try_run(self.store.as_ref(), &database, at)?;
        self.record_indexes(&database, &query);
        let aggregate_fields = if enterprise {
            aggregations.compute_enterprise(&results.documents, query.projection())?
        } else {
            aggregations.compute(&results.documents)
        };
        // The official emulator also sets `done: true`, a field the published protos lack.
        responses.push(Ok(RunAggregationQueryResponse {
            result: Some(AggregationResult { aggregate_fields }),
            read_time: Some(at.to_timestamp()),
            ..RunAggregationQueryResponse::default()
        }));
        Ok(Response::new(Box::pin(tokio_stream::iter(responses))))
    }

    async fn write(
        &self,
        request: Request<Streaming<WriteRequest>>,
    ) -> Result<Response<BoxStream<WriteResponse>>, Status> {
        let caller = self.caller(request.metadata(), None)?;
        let mut requests = request.into_inner();
        let (responses, receiver) = mpsc::channel(16);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(status) = service
                .serve_write_stream(&mut requests, &responses, &caller)
                .await
            {
                let _ = responses.send(Err(status)).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn listen(
        &self,
        request: Request<Streaming<ListenRequest>>,
    ) -> Result<Response<BoxStream<ListenResponse>>, Status> {
        let caller = self.caller(request.metadata(), None)?;
        let mut requests = request.into_inner();
        let (responses, receiver) = mpsc::channel(256);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(status) = service
                .serve_listen(&mut requests, &responses, &caller)
                .await
            {
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
        let database = names::database(&req.database)?;
        // A standard-edition database refuses pipelines before reading the header, as on the
        // official emulator.
        if !self.enterprise {
            return Err(Status::invalid_argument(
                "ExecutePipeline requires the Database Edition to be `enterprise`.",
            ));
        }
        self.caller(&metadata, Some(&database))?;
        let responses = self.execute(&database, &req)?;
        Ok(Response::new(Box::pin(tokio_stream::iter(
            responses.into_iter().map(Ok),
        ))))
    }

    async fn begin_transaction(
        &self,
        request: Request<BeginTransactionRequest>,
    ) -> Result<Response<BeginTransactionResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        self.caller(&metadata, Some(&database))?;
        Ok(Response::new(BeginTransactionResponse {
            transaction: self.begin(&database, req.options.as_ref())?,
        }))
    }

    async fn rollback(&self, request: Request<RollbackRequest>) -> Result<Response<()>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        self.caller(&metadata, Some(&database))?;
        self.transactions.rollback(&database, &req.transaction)?;
        Ok(Response::new(()))
    }

    async fn batch_write(
        &self,
        request: Request<BatchWriteRequest>,
    ) -> Result<Response<BatchWriteResponse>, Status> {
        let (metadata, _, req) = request.into_parts();
        let database = names::database(&req.database)?;
        let caller = self.caller(&metadata, Some(&database))?;
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
        // An invalid write fails the whole request, as on the official emulator: the names of
        // every write first, then their values (`tools/oracle/commit_checks.py`).
        let targets = req
            .writes
            .iter()
            .map(|write| writes::check_names(&database, write))
            .collect::<Result<Vec<_>, _>>()?;
        for (write, path) in req.writes.iter().zip(&targets) {
            writes::check_values(path, write)?;
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
                .write(
                    &database,
                    None,
                    std::slice::from_ref(write),
                    self.guard(&caller, &database).as_ref(),
                    Verifies::Checked,
                )
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
        let caller = self.caller(&metadata, Some(&parent.database))?;
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

/// A read mask, whose paths are checked as an update mask's are (`validate::mask_paths`,
/// `tools/oracle/rest.py`, #262).
fn parse_mask(mask: Option<&DocumentMask>) -> Result<Option<Vec<FieldPath>>, Status> {
    mask.map(|m| validate::mask_paths(&m.field_paths))
        .transpose()
}

/// The read mask of a write that returns the document (UpdateDocument, CreateDocument), checked
/// as the official emulator does: after the names of the write (its document, its fields' names,
/// its update mask, its transforms) and before its values and its precondition, so that a write
/// whose mask is invalid writes nothing (`tools/oracle/rest.py`, #262).
fn read_mask(
    database: &str,
    write: &Write,
    mask: Option<&DocumentMask>,
) -> Result<Option<Vec<FieldPath>>, Status> {
    writes::check_names(database, write)?;
    parse_mask(mask)
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

/// The orders of ListDocuments' `order_by`, as the official emulator reads them: clauses
/// separated by commas, each a field path (backticks quote, as in any path) and `asc` or `desc`
/// in any case, with whitespace around; anything else is "Invalid order by clause".
fn list_orders(order_by: &str) -> Result<Vec<structured_query::Order>, Status> {
    let invalid = || Status::invalid_argument(format!("Invalid order by clause \"{order_by}\"."));
    let mut orders = Vec::new();
    if order_by.is_empty() {
        return Ok(orders);
    }
    let mut chars = order_by.chars().peekable();
    let skip_whitespace = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
    };
    loop {
        skip_whitespace(&mut chars);
        let mut path = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() || c == ',' {
                break;
            }
            path.push(c);
            chars.next();
            if c == '`' {
                while let Some(c) = chars.next() {
                    path.push(c);
                    match c {
                        '`' => break,
                        '\\' => path.extend(chars.next()),
                        _ => {}
                    }
                }
            }
        }
        if !property_path::well_formed(&path) {
            return Err(invalid());
        }
        skip_whitespace(&mut chars);
        let mut word = String::new();
        while let Some(c) = chars.next_if(char::is_ascii_alphabetic) {
            word.push(c);
        }
        let direction = match word.to_ascii_lowercase().as_str() {
            "" | "asc" => structured_query::Direction::Ascending,
            "desc" => structured_query::Direction::Descending,
            _ => return Err(invalid()),
        };
        skip_whitespace(&mut chars);
        orders.push(structured_query::Order {
            field: Some(structured_query::FieldReference { field_path: path }),
            direction: direction as i32,
        });
        match chars.next() {
            None => return Ok(orders),
            Some(',') => {}
            Some(_) => return Err(invalid()),
        }
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
