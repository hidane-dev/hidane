//! `google.firestore.v1.Firestore`.
//!
//! Implemented: GetDocument, ListDocuments, CreateDocument, UpdateDocument, DeleteDocument,
//! BatchGetDocuments, Commit, BatchWrite, ListCollectionIds. The rest answer `UNIMPLEMENTED`
//! through the generated default stubs until their issues land.
//!
//! Behaviour follows the official emulator as recorded in `tests/fixtures/document_writes.json`
//! (`tools/oracle/document_writes.py`), including its error messages, except where the official
//! message prints internal Datastore keys (docs/parity-exceptions.md).

mod names;
mod validate;
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
    field_path::{FieldPath, project},
    path::ResourcePath,
    store::{ListItem, ListOptions, ReadTime, Store, StoreError, StoredDocument},
};
use hidane_proto::google::{
    firestore::v1::{
        BatchGetDocumentsRequest, BatchGetDocumentsResponse, BatchWriteRequest, BatchWriteResponse,
        CommitRequest, CommitResponse, CreateDocumentRequest, DeleteDocumentRequest, Document,
        DocumentMask, GetDocumentRequest, ListCollectionIdsRequest, ListCollectionIdsResponse,
        ListDocumentsRequest, ListDocumentsResponse, Precondition, UpdateDocumentRequest, Write,
        WriteResult, batch_get_documents_request, batch_get_documents_response,
        firestore_server::Firestore, get_document_request, list_collection_ids_request,
        list_documents_request, precondition::ConditionType, write::Operation,
    },
    rpc,
};
use prost_types::Timestamp;
use tonic::{Request, Response, Status, async_trait, codegen::BoxStream};

use self::names::Name;

const METADATA_ADMIN: &str = "Metadata operations require admin authentication.";
const BATCH_WRITE_ADMIN: &str = "Batch writes require admin authentication.";

pub struct FirestoreService {
    store: Arc<dyn Store>,
}

impl FirestoreService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
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
            Ok(commit) => Ok((commit.commit_time, results)),
            Err(err) => Err(failure.unwrap_or_else(|| store_error(&err))),
        }
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
        let req = request.into_inner();
        let name = names::document(&req.name)?;
        let at = match &req.consistency_selector {
            Some(get_document_request::ConsistencySelector::Transaction(_)) => {
                return Err(invalid_transaction());
            }
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
        let admin = is_admin(&request);
        let req = request.into_inner();
        let parent = names::parent(&req.parent)?;
        names::validate_id(&req.collection_id)?;
        if req.show_missing && !admin {
            return Err(Status::permission_denied(METADATA_ADMIN));
        }
        if !matches!(req.order_by.as_str(), "" | "__name__" | "__name__ asc") {
            return Err(Status::unimplemented(
                "ListDocuments order_by other than __name__ is not implemented yet",
            ));
        }
        let at = match &req.consistency_selector {
            Some(list_documents_request::ConsistencySelector::Transaction(_)) => {
                return Err(invalid_transaction());
            }
            Some(list_documents_request::ConsistencySelector::ReadTime(ts)) => {
                self.read_time(&parent.database, Some(ts))?
            }
            None => self.read_time(&parent.database, None)?,
        };
        let mask = parse_mask(req.mask.as_ref())?;
        let collection = parent.path.child(req.collection_id.clone());
        let after = decode_token(&req.page_token)?
            .map(|p| ResourcePath::parse(&p).ok_or_else(invalid_token))
            .transpose()?;
        if after
            .as_ref()
            .is_some_and(|a| a.parent().as_ref() != Some(&collection))
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
        self.store.list_collection(
            &parent.database,
            &collection,
            at,
            ListOptions {
                after: after.as_ref(),
                include_missing: req.show_missing,
            },
            &mut |item| {
                if documents.len() == limit {
                    more = true;
                    return ControlFlow::Break(());
                }
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
                ControlFlow::Continue(())
            },
        );
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
        let req = request.into_inner();
        let parent = names::parent(&req.parent)?;
        names::validate_id(&req.collection_id)?;
        let id = if req.document_id.is_empty() {
            auto_id()
        } else {
            names::validate_id(&req.document_id)?;
            req.document_id.clone()
        };
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
        let (commit_time, _) = self.commit_writes(&parent.database, &[write])?;
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
        let req = request.into_inner();
        let document = req
            .document
            .ok_or_else(|| Status::invalid_argument("A document is required."))?;
        let name = names::document(&document.name)?;
        let write = Write {
            operation: Some(Operation::Update(document)),
            update_mask: req.update_mask,
            current_document: req.current_document,
            ..Write::default()
        };
        let (commit_time, _) = self.commit_writes(&name.database, &[write])?;
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
        let req = request.into_inner();
        let name = names::document(&req.name)?;
        let write = Write {
            operation: Some(Operation::Delete(req.name)),
            current_document: req.current_document,
            ..Write::default()
        };
        self.commit_writes(&name.database, &[write])?;
        Ok(Response::new(()))
    }

    async fn batch_get_documents(
        &self,
        request: Request<BatchGetDocumentsRequest>,
    ) -> Result<Response<BoxStream<BatchGetDocumentsResponse>>, Status> {
        let req = request.into_inner();
        let database = names::database(&req.database)?;
        let at = match &req.consistency_selector {
            Some(batch_get_documents_request::ConsistencySelector::Transaction(_)) => {
                return Err(invalid_transaction());
            }
            Some(batch_get_documents_request::ConsistencySelector::NewTransaction(_)) => {
                return Err(transactions_unimplemented());
            }
            Some(batch_get_documents_request::ConsistencySelector::ReadTime(ts)) => {
                self.read_time(&database, Some(ts))?
            }
            None => self.read_time(&database, None)?,
        };
        let mask = parse_mask(req.mask.as_ref())?;
        let mut responses = Vec::with_capacity(req.documents.len());
        // Answers come back in request order, all with the same read time.
        for name in &req.documents {
            let parsed = names::document(name)?;
            if parsed.database != database {
                return Err(Status::invalid_argument(format!(
                    "Document \"{name}\" is not in database \"{database}\"."
                )));
            }
            let result = match self.store.get(&database, &parsed.path, at) {
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
        let req = request.into_inner();
        let database = names::database(&req.database)?;
        if !req.transaction.is_empty() {
            return Err(invalid_transaction());
        }
        // The official emulator answers an empty commit with an empty response (no commit time).
        if req.writes.is_empty() {
            return Ok(Response::new(CommitResponse::default()));
        }
        let (commit_time, write_results) = self.commit_writes(&database, &req.writes)?;
        Ok(Response::new(CommitResponse {
            write_results,
            commit_time: Some(commit_time.to_timestamp()),
        }))
    }

    async fn batch_write(
        &self,
        request: Request<BatchWriteRequest>,
    ) -> Result<Response<BatchWriteResponse>, Status> {
        if !is_admin(&request) {
            return Err(Status::permission_denied(BATCH_WRITE_ADMIN));
        }
        let req = request.into_inner();
        let database = names::database(&req.database)?;
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
        // Writes are independent: a failing one is reported and the others still apply.
        let mut write_results = Vec::with_capacity(req.writes.len());
        let mut status = Vec::with_capacity(req.writes.len());
        if !req.writes.is_empty() {
            self.store
                .commit(&database, &mut |batch| {
                    write_results.clear();
                    status.clear();
                    for write in &req.writes {
                        match writes::apply(batch, &database, write) {
                            Ok(result) => {
                                write_results.push(result);
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
                    Ok(())
                })
                .map_err(|err| store_error(&err))?;
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
        if !is_admin(&request) {
            return Err(Status::permission_denied(METADATA_ADMIN));
        }
        let req = request.into_inner();
        let parent = names::parent(&req.parent)?;
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
        let more = ids.len() > limit;
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

fn to_document(database: &str, doc: &StoredDocument, mask: Option<&[FieldPath]>) -> Document {
    let fields = doc.fields();
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

/// `Authorization: Bearer owner` (what the server SDKs send to an emulator) and Google OAuth
/// access tokens are administrators, as on the official emulator (#24 handles the rest).
fn is_admin<T>(request: &Request<T>) -> bool {
    request
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "Bearer owner" || v.starts_with("Bearer ya29."))
}

/// What the official emulator answers for a transaction it does not know. Until
/// transactions exist (#17), every transaction is unknown.
fn invalid_transaction() -> Status {
    Status::invalid_argument("Invalid transaction.")
}

fn transactions_unimplemented() -> Status {
    Status::unimplemented(
        "Transactions are not implemented yet (https://github.com/hidane-dev/hidane/issues/17)",
    )
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
