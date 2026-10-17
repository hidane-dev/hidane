//! Datastore mode (#81, [ADR 0009](../../../../docs/adr/0009-datastore-mode.md)): with
//! `--database-mode datastore-mode`, hidane serves `google.datastore.v1.Datastore` over gRPC and
//! REST in place of the Firestore API, which it refuses as the official emulator does
//! ([`FIRESTORE_REFUSED`]).
//!
//! Entities live here, apart from the Firestore engine's documents: keys and values are
//! Datastore's ([`keys`], [`values`]). Transactions are the Firestore engine's
//! ([`Transactions`]): the official emulator locks, waits and expires them alike in both modes,
//! with the same messages, but for a write outside a transaction, which waits 60 s for a lock
//! here ([`crate::firestore::transactions::OUTSIDE_LOCK_TIMEOUT`]); a key's lock is kept under
//! [`keys::lock_path`]. What is served so far:
//! `Lookup`, `Commit`, `BeginTransaction`, `Rollback`, `AllocateIds` and `ReserveIds`
//! (`tests/fixtures/datastore_entities.json`; commits and lookups refused in the official
//! emulator's order, `tests/fixtures/datastore_commit_checks.json`), and structured `RunQuery` and
//! `RunAggregationQuery` ([`query`], `tests/fixtures/datastore_queries.json`), refused in the
//! official emulator's order ([`checks`], `tests/fixtures/datastore_checks.json`), and GQL
//! ([`gql`], `tests/fixtures/datastore_gql.json`), with the indexes `--require_indexes` asks for
//! ([`indexes`], `tests/fixtures/datastore_indexes.json`).

mod checks;
mod export;
mod gql;
mod indexes;
pub(crate) mod keys;
mod query;
pub(crate) mod rest;
mod values;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_core::store::ReadTime;
use hidane_proto::{
    google::datastore::v1::{
        AllocateIdsRequest, AllocateIdsResponse, BeginTransactionRequest, BeginTransactionResponse,
        CommitRequest, CommitResponse, Entity, EntityResult, LookupRequest, LookupResponse,
        MutationResult, PartitionId, ReadOptions, ReserveIdsRequest, ReserveIdsResponse,
        RollbackRequest, RollbackResponse, RunAggregationQueryRequest, RunAggregationQueryResponse,
        RunQueryRequest, RunQueryResponse, TransactionOptions, Value, aggregation_query,
        commit_request, datastore_server::Datastore, mutation, read_options,
        run_aggregation_query_request, run_query_request, transaction_options, value::ValueType,
    },
    ordered::Properties,
};
use prost_types::Timestamp;
use tonic::{Request, Response, Status};

use crate::firestore::transactions::{Mode, Transactions};
use keys::{Database, Last, Located, Rules};

/// What the Firestore API answers in Datastore mode, over gRPC and REST.
pub const FIRESTORE_REFUSED: &str = "Database mode not set to Firestore Mode. If using the gCloud CLI, please specify the --database-mode flag.";

/// The Firestore API's answer in Datastore mode.
pub fn firestore_refused() -> Status {
    Status::invalid_argument(FIRESTORE_REFUSED)
}

/// What the Datastore API answers in Firestore Native mode, over gRPC and REST, once it has read
/// the request's project (`tests/fixtures/datastore_projects.json`).
pub const DATASTORE_REFUSED: &str = "Database mode not set to Datastore Mode. If using the gCloud CLI, please specify the --database-mode flag.";

/// The Datastore service, sharing its state between gRPC and REST.
#[derive(Clone, Default)]
pub struct DatastoreService {
    state: Arc<State>,
}

struct State {
    data: Mutex<Data>,
    transactions: Transactions,
    /// `--require_indexes`: the indexes queries need.
    enforcement: Option<indexes::Enforcement>,
    /// Firestore Native mode: every request is refused once its project is read.
    refused: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            data: Mutex::default(),
            // A write outside a transaction waits longer for a lock than in Firestore Native
            // mode (`tests/fixtures/datastore_locks.json`).
            transactions: Transactions::default()
                .with_outside_lock_timeout(crate::firestore::transactions::OUTSIDE_LOCK_TIMEOUT),
            enforcement: None,
            refused: false,
        }
    }
}

#[derive(Default)]
struct Data {
    databases: HashMap<String, DatabaseData>,
    /// The last commit's time, in microseconds: commits are strictly ordered in time.
    last_micros: i64,
}

#[derive(Default)]
struct DatabaseData {
    entities: BTreeMap<Located, Stored>,
    /// The version each deleted entity was deleted at, which a lookup tells of it.
    deleted: BTreeMap<Located, i64>,
    /// The last ID allocated: one counter per database, whatever the kind or the namespace.
    allocated: u64,
    /// When the database was first used, in microseconds: reads before it are too old.
    created: i64,
}

impl DatabaseData {
    /// Writes `entity` under `located`, or deletes what is there at version `micros`.
    fn put(&mut self, located: Located, entity: Option<Stored>, micros: i64) {
        match entity {
            Some(entity) => {
                self.deleted.remove(&located);
                self.entities.insert(located, entity);
            }
            None => {
                if self.entities.remove(&located).is_some() {
                    self.deleted.insert(located, micros);
                }
            }
        }
    }
}

#[derive(Clone)]
struct Stored {
    properties: Properties,
    version: i64,
    created: i64,
    updated: i64,
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

fn timestamp(micros: i64) -> Timestamp {
    #[allow(clippy::cast_possible_truncation)]
    Timestamp {
        seconds: micros.div_euclid(1_000_000),
        nanos: (micros.rem_euclid(1_000_000) * 1_000) as i32,
    }
}

/// The `counter`-th ID a database allocates: 2⁵² plus the counter's 51 low bits reversed, so
/// that IDs are spread over their range, as the official emulator spreads them.
fn scattered(counter: u64) -> i64 {
    let reversed = (counter & ((1 << 51) - 1)).reverse_bits() >> (64 - 51);
    #[allow(clippy::cast_possible_wrap)]
    let id = ((1u64 << 52) + reversed) as i64;
    id
}

/// An entity's cursor in a lookup's results: its key, opaque to clients.
fn cursor(located: &Located) -> Vec<u8> {
    let mut out = b"hidane:".to_vec();
    out.extend_from_slice(located.namespace.as_bytes());
    for (kind, id) in &located.path {
        out.push(0);
        out.extend_from_slice(kind.as_bytes());
        out.push(0);
        match id {
            keys::Id::Id(n) => out.extend_from_slice(n.to_string().as_bytes()),
            keys::Id::Name(name) => out.extend_from_slice(name.as_bytes()),
        }
    }
    out
}

impl Data {
    fn database(&mut self, database: &Database) -> &mut DatabaseData {
        let created = now_micros();
        self.databases
            .entry(database.name())
            .or_insert_with(|| DatabaseData {
                created,
                ..DatabaseData::default()
            })
    }

    /// A commit's time, after every earlier one.
    fn next_micros(&mut self) -> i64 {
        self.last_micros = now_micros().max(self.last_micros + 1);
        self.last_micros
    }

    /// The time of a read: now, and never before the last commit.
    fn read_micros(&self) -> i64 {
        now_micros().max(self.last_micros)
    }
}

impl DatastoreService {
    fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        self.state
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// `--require_indexes`: queries need the indexes `file` (`--index_file`) defines.
    #[must_use]
    pub fn with_indexes(file: Option<std::path::PathBuf>) -> Self {
        Self {
            state: Arc::new(State {
                enforcement: Some(indexes::Enforcement::new(file)),
                ..State::default()
            }),
        }
    }

    /// The Datastore API of Firestore Native mode: it reads a request's project, then refuses it
    /// ([`DATASTORE_REFUSED`]), as the official emulator does.
    pub fn refusing() -> Self {
        Self {
            state: Arc::new(State {
                refused: true,
                ..State::default()
            }),
        }
    }

    /// A request's database, its project read first: an app ID, then, in Firestore Native mode,
    /// refused, then a project ID (`tests/fixtures/datastore_projects.json`).
    fn admit(&self, project: String, database: String) -> Result<Database, Status> {
        keys::check_app_id(&project)?;
        if self.state.refused {
            return Err(Status::invalid_argument(DATASTORE_REFUSED));
        }
        keys::check_project_id(&project)?;
        Ok(Database { project, database })
    }

    /// Every entity of every database, and every open transaction (`POST /reset`); the index
    /// file is read again.
    pub fn reset(&self) {
        *self.data() = Data::default();
        self.state.transactions.clear();
        if let Some(enforcement) = &self.state.enforcement {
            enforcement.reset();
        }
    }

    /// With `--require_indexes`, the indexes `database` read from the file.
    fn indexes(&self, database: &Database) -> Result<Option<Arc<Vec<indexes::Composite>>>, Status> {
        self.state
            .enforcement
            .as_ref()
            .map(|enforcement| enforcement.indexes(database))
            .transpose()
    }

    /// A read time a request names: not before the database was first used.
    fn check_read_time(&self, database: &Database, time: &Timestamp) -> Result<ReadTime, Status> {
        let at = ReadTime::from_timestamp(time);
        let mut data = self.data();
        let now = data.read_micros();
        if at.0 > now {
            return Err(Status::invalid_argument(
                "The requested 'read_time' cannot be in the future.",
            ));
        }
        if at.0 < data.database(database).created {
            return Err(Status::failed_precondition(
                "The requested 'read_time' is too old.",
            ));
        }
        Ok(at)
    }

    fn begin(
        &self,
        database: &Database,
        options: Option<&TransactionOptions>,
    ) -> Result<Vec<u8>, Status> {
        let name = database.name();
        let mode = match options.and_then(|o| o.mode.as_ref()) {
            None => Mode::ReadWrite,
            Some(transaction_options::Mode::ReadWrite(read_write)) => {
                if !read_write.previous_transaction.is_empty() {
                    self.state
                        .transactions
                        .check_retry(&name, &read_write.previous_transaction)?;
                }
                Mode::ReadWrite
            }
            Some(transaction_options::Mode::ReadOnly(read_only)) => match &read_only.read_time {
                Some(time) => Mode::ReadOnly(self.check_read_time(database, time)?),
                None => Mode::ReadOnly(ReadTime(self.data().read_micros())),
            },
        };
        Ok(self.state.transactions.begin(&name, mode))
    }
}

/// What a query's read options say: the transaction to report a new one in.
struct Reading {
    new_transaction: Vec<u8>,
}

impl DatastoreService {
    /// A query's partition: its project must be the request's; its namespace is the query's.
    fn namespace(partition: Option<PartitionId>, database: &Database) -> Result<String, Status> {
        let partition = partition.unwrap_or_default();
        keys::check_partition(
            &hidane_proto::google::datastore::v1::Key {
                partition_id: Some(partition.clone()),
                path: Vec::new(),
            },
            database,
        )?;
        keys::check_namespace(&partition.namespace_id)?;
        Ok(partition.namespace_id)
    }

    /// A query's read options: a read-write transaction locks `kind` (a query without one is an
    /// internal error there); eventual consistency reads as strong. A read time is checked by
    /// [`Self::read_at`].
    fn read_query(
        &self,
        options: Option<ReadOptions>,
        database: &Database,
        kind: Option<&str>,
    ) -> Result<Reading, Status> {
        let mut reading = Reading {
            new_transaction: Vec::new(),
        };
        // A read-write transaction locks the kind it queries; a query without a kind is an
        // internal error there on the official emulator.
        let lock = |transaction: &[u8]| {
            let mode = self
                .state
                .transactions
                .read(&database.name(), transaction, &[], kind)?;
            if kind.is_none() && mode == Mode::ReadWrite {
                return Err(Status::unknown(""));
            }
            Ok(mode)
        };
        match options.and_then(|o| o.consistency_type) {
            None | Some(read_options::ConsistencyType::ReadConsistency(_)) => {}
            Some(read_options::ConsistencyType::ReadTime(_)) => {}
            Some(read_options::ConsistencyType::Transaction(transaction)) => {
                lock(&transaction)?;
            }
            Some(read_options::ConsistencyType::NewTransaction(options)) => {
                let transaction = self.begin(database, Some(&options))?;
                lock(&transaction)?;
                reading.new_transaction = transaction;
            }
        }
        Ok(reading)
    }

    /// A query's read time: checked once everything else is.
    fn read_at(&self, options: Option<&ReadOptions>, database: &Database) -> Result<(), Status> {
        if let Some(read_options::ConsistencyType::ReadTime(time)) =
            options.and_then(|o| o.consistency_type.as_ref())
        {
            self.check_read_time(database, time)?;
        }
        Ok(())
    }
}

/// How a commit is made.
enum Commit {
    NonTransactional,
    Transactional(Vec<u8>),
}

/// A mutation, checked.
enum Checked {
    Write {
        op: Op,
        key: keys::Checked,
        properties: Properties,
        mask: Option<Vec<String>>,
        late: Late,
    },
    Delete(Located, Late),
}

/// What is checked of a mutation once every mutation has been read, in order and next to what
/// is stored ([`apply`]).
struct Late {
    /// The version the mutation was made at: its `base_version`, or its `update_time` in
    /// microseconds.
    required: Option<i64>,
    /// Its `conflict_resolution_strategy`.
    resolution: i32,
    nested_arrays: bool,
    index_entries: usize,
    size: usize,
}

impl Late {
    fn of(
        mutation: &hidane_proto::google::datastore::v1::Mutation,
        written: Option<(&keys::Checked, &Properties)>,
    ) -> Self {
        let required = match &mutation.conflict_detection_strategy {
            Some(mutation::ConflictDetectionStrategy::BaseVersion(v)) => Some(*v),
            Some(mutation::ConflictDetectionStrategy::UpdateTime(t)) => {
                Some(t.seconds * 1_000_000 + i64::from(t.nanos) / 1_000)
            }
            None => None,
        };
        Self {
            required,
            resolution: mutation.conflict_resolution_strategy,
            nested_arrays: written.is_some_and(|(_, p)| values::nested_arrays(p)),
            index_entries: written.map_or(0, |(_, p)| values::index_entries(p)),
            size: written.map_or(0, |(key, p)| values::entity_size(key, p)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Insert,
    Update,
    Upsert,
}

#[tonic::async_trait]
impl Datastore for DatastoreService {
    async fn lookup(
        &self,
        request: Request<LookupRequest>,
    ) -> Result<Response<LookupResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        self.indexes(&database)?;
        let mut wanted: Vec<Located> = Vec::with_capacity(req.keys.len());
        for key in &req.keys {
            let located = keys::check(key, &database, Rules::Lookup, Last::Complete)?
                .complete()
                .expect("checked complete");
            if !wanted.contains(&located) {
                wanted.push(located);
            }
        }
        read_options_shape(req.read_options.as_ref())?;
        past_epoch(req.read_options.as_ref())?;
        if let Some(mask) = &req.property_mask {
            values::check_paths(&mask.paths)?;
        }
        let mut response = LookupResponse::default();
        match req.read_options.and_then(|o| o.consistency_type) {
            None => {}
            Some(read_options::ConsistencyType::ReadConsistency(consistency)) => {
                if consistency == read_options::ReadConsistency::Eventual as i32 {
                    return Err(Status::invalid_argument(
                        "Firestore emulator does not support eventual consistency options.",
                    ));
                }
            }
            Some(read_options::ConsistencyType::ReadTime(time)) => {
                self.check_read_time(&database, &time)?;
            }
            Some(read_options::ConsistencyType::Transaction(transaction)) => {
                let paths: Vec<_> = wanted.iter().map(keys::lock_path).collect();
                self.state
                    .transactions
                    .read(&database.name(), &transaction, &paths, None)?;
            }
            Some(read_options::ConsistencyType::NewTransaction(options)) => {
                let transaction = self.begin(&database, Some(&options))?;
                let paths: Vec<_> = wanted.iter().map(keys::lock_path).collect();
                self.state
                    .transactions
                    .read(&database.name(), &transaction, &paths, None)?;
                response.transaction = transaction;
            }
        }
        let mut data = self.data();
        let read = data.read_micros();
        let stored = data.database(&database);
        for located in &wanted {
            let key = Some(database.key(located));
            match stored.entities.get(located) {
                Some(entity) => {
                    let mut found = Entity {
                        key,
                        properties: entity.properties.clone(),
                    };
                    if let Some(mask) = &req.property_mask {
                        found.properties = values::masked(&found, mask);
                    }
                    response.found.push(EntityResult {
                        entity: Some(found),
                        cursor: cursor(located),
                        version: entity.version,
                        create_time: Some(timestamp(entity.created)),
                        update_time: Some(timestamp(entity.updated)),
                    });
                }
                None => response.missing.push(EntityResult {
                    entity: Some(Entity {
                        key,
                        properties: Properties::new(),
                    }),
                    version: stored.deleted.get(located).copied().unwrap_or_default(),
                    ..EntityResult::default()
                }),
            }
        }
        response.read_time = Some(timestamp(read));
        Ok(Response::new(response))
    }

    async fn begin_transaction(
        &self,
        request: Request<BeginTransactionRequest>,
    ) -> Result<Response<BeginTransactionResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        self.indexes(&database)?;
        let transaction = self.begin(&database, req.transaction_options.as_ref())?;
        Ok(Response::new(BeginTransactionResponse { transaction }))
    }

    async fn commit(
        &self,
        request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        self.indexes(&database)?;
        let name = database.name();
        let transaction = match req.transaction_selector {
            Some(commit_request::TransactionSelector::Transaction(t)) => Some(t),
            Some(commit_request::TransactionSelector::SingleUseTransaction(options)) => {
                Some(self.begin(&database, Some(&options))?)
            }
            None => None,
        };
        let how = if req.mode == commit_request::Mode::NonTransactional as i32 {
            if transaction.is_some() {
                return Err(Status::invalid_argument(
                    "non-transaction commit cannot specify a transaction",
                ));
            }
            Commit::NonTransactional
        } else {
            match transaction {
                Some(t) => Commit::Transactional(t),
                None => {
                    return Err(Status::invalid_argument(
                        "transactional commit requires a transaction",
                    ));
                }
            }
        };
        let any = !req.mutations.is_empty();
        // Each mutation's operation, key, properties, partition and mask, in order; what
        // `Late` keeps comes once they all are read.
        let mut checked = Vec::with_capacity(req.mutations.len());
        for mutation in &req.mutations {
            let (key, write) = match &mutation.operation {
                None => return Err(Status::invalid_argument("Mutation is missing operation.")),
                Some(mutation::Operation::Delete(key)) => (key, None),
                Some(
                    mutation::Operation::Insert(e)
                    | mutation::Operation::Update(e)
                    | mutation::Operation::Upsert(e),
                ) => {
                    let op = match &mutation.operation {
                        Some(mutation::Operation::Insert(_)) => Op::Insert,
                        Some(mutation::Operation::Update(_)) => Op::Update,
                        _ => Op::Upsert,
                    };
                    let Some(key) = &e.key else {
                        return Err(Status::invalid_argument("Entity is missing key."));
                    };
                    (key, Some((op, &e.properties)))
                }
            };
            let last = match write {
                Some((Op::Insert | Op::Upsert, _)) => Last::Either,
                _ => Last::Complete,
            };
            let checked_key = keys::check_unpartitioned(key, Rules::Write, last)?;
            if let Some((_, properties)) = write {
                values::check_properties(properties)?;
            }
            keys::check_partition(key, &database)?;
            if mutation.conflict_detection_strategy.is_some() && checked_key.complete().is_none() {
                return Err(Status::invalid_argument(
                    "conflict detection is not allowed for incomplete keys",
                ));
            }
            let mask = mutation.property_mask.as_ref().map(|m| m.paths.clone());
            if let Some(paths) = &mask {
                values::check_paths(paths)?;
            }
            let late = Late::of(mutation, write.map(|(_, p)| (&checked_key, p)));
            checked.push(match write {
                None => Checked::Delete(checked_key.complete().expect("checked complete"), late),
                Some((op, properties)) => {
                    let mut properties = properties.clone();
                    values::normalize(&mut properties, &database);
                    Checked::Write {
                        op,
                        key: checked_key,
                        properties,
                        mask,
                        late,
                    }
                }
            });
        }
        let complete: Vec<Located> = checked
            .iter()
            .filter_map(|c| match c {
                Checked::Write { key, .. } => key.complete(),
                Checked::Delete(located, _) => Some(located.clone()),
            })
            .collect();
        if matches!(how, Commit::NonTransactional) {
            let mut seen = HashSet::new();
            if !complete.iter().all(|located| seen.insert(located)) {
                return Err(Status::invalid_argument(
                    "A non-transactional commit may not contain multiple mutations affecting the same entity.",
                ));
            }
        }
        if let Commit::Transactional(t) = &how {
            self.state.transactions.check_commit(&name, t, any)?;
        }
        let targets: Vec<_> = complete.iter().map(keys::lock_path).collect();
        let owner = match &how {
            Commit::Transactional(t) => Some(t.as_slice()),
            Commit::NonTransactional => None,
        };
        let transactional = owner.is_some();
        let count = checked.len();
        let state = Arc::clone(&self.state);
        let response = self
            .state
            .transactions
            .commit(&name, owner, &targets, move || {
                let mut data = state.data.lock().unwrap_or_else(PoisonError::into_inner);
                apply(&mut data, &database, checked, transactional)
            })
            .await?;
        let (results, micros) = response;
        let mut response = CommitResponse::default();
        if count > 0 || transactional {
            response.commit_time = Some(timestamp(micros));
        }
        if count > 0 && transactional {
            response.commit_version = micros;
        }
        response.mutation_results = results;
        Ok(Response::new(response))
    }

    async fn rollback(
        &self,
        request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        if req.transaction.is_empty() {
            return Err(Status::invalid_argument("transaction required"));
        }
        self.indexes(&database)?;
        self.state
            .transactions
            .rollback(&database.name(), &req.transaction)?;
        Ok(Response::new(RollbackResponse {}))
    }

    async fn allocate_ids(
        &self,
        request: Request<AllocateIdsRequest>,
    ) -> Result<Response<AllocateIdsResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        self.indexes(&database)?;
        let checked = req
            .keys
            .iter()
            .map(|key| keys::check(key, &database, Rules::Write, Last::Incomplete))
            .collect::<Result<Vec<_>, _>>()?;
        let mut data = self.data();
        let stored = data.database(&database);
        let keys = checked
            .iter()
            .map(|key| {
                stored.allocated += 1;
                database.key(&key.with_id(scattered(stored.allocated)))
            })
            .collect();
        Ok(Response::new(AllocateIdsResponse { keys }))
    }

    async fn run_query(
        &self,
        request: Request<RunQueryRequest>,
    ) -> Result<Response<RunQueryResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        let defined = self.indexes(&database)?;
        // GQL is read first, a key in it without a partition in the request's namespace; then the
        // request's partition and read options are checked, then the query.
        let requested = req
            .partition_id
            .as_ref()
            .map(|p| p.namespace_id.clone())
            .unwrap_or_default();
        let mut echo = None;
        let query = match req.query_type {
            Some(run_query_request::QueryType::Query(query)) => query,
            Some(run_query_request::QueryType::GqlQuery(text)) => {
                match gql::read(&text, &requested, gql::Expect::Entities)? {
                    gql::Read::Entities(query) => {
                        echo = Some(query.clone());
                        query
                    }
                    gql::Read::Aggregation(_) => unreachable!("an entity query was expected"),
                }
            }
            None => {
                return Err(Status::invalid_argument(
                    "one of fields Query.query and Query.gql_query must be set",
                ));
            }
        };
        let namespace = Self::namespace(req.partition_id, &database)?;
        read_options_shape(req.read_options.as_ref())?;
        let refused = checks::entity_query(
            &query,
            &namespace,
            &database,
            past_epoch(req.read_options.as_ref()),
        )?;
        let plan = query::plan(&query, &database, &namespace)?;
        let reading = self.read_query(req.read_options.clone(), &database, plan.kind.as_deref())?;
        if let Some(defined) = &defined {
            checks::kindless(&query)?;
            indexes::check(&query, &[], defined)?;
        }
        if let Some(refused) = refused {
            return Err(refused);
        }
        self.read_at(req.read_options.as_ref(), &database)?;
        let mut data = self.data();
        let read = data.read_micros();
        let stored = data.database(&database);
        let batch = query::metadata(&plan, &stored.entities, &database, read)
            .unwrap_or_else(|| query::run(&plan, &stored.entities, &database, read));
        Ok(Response::new(RunQueryResponse {
            batch: Some(batch),
            query: echo,
            transaction: reading.new_transaction,
            ..RunQueryResponse::default()
        }))
    }

    async fn run_aggregation_query(
        &self,
        request: Request<RunAggregationQueryRequest>,
    ) -> Result<Response<RunAggregationQueryResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        let defined = self.indexes(&database)?;
        let requested = req
            .partition_id
            .as_ref()
            .map(|p| p.namespace_id.clone())
            .unwrap_or_default();
        let aggregation = match req.query_type {
            Some(run_aggregation_query_request::QueryType::AggregationQuery(a)) => a,
            Some(run_aggregation_query_request::QueryType::GqlQuery(text)) => {
                match gql::read(&text, &requested, gql::Expect::Aggregation)? {
                    gql::Read::Aggregation(aggregation) => aggregation,
                    gql::Read::Entities(_) => unreachable!("an aggregation was expected"),
                }
            }
            None => {
                return Err(Status::invalid_argument(
                    "one of fields aggregation_query and gql_query must be set",
                ));
            }
        };
        let nested = match &aggregation.query_type {
            Some(aggregation_query::QueryType::NestedQuery(query)) => query.clone(),
            None => hidane_proto::google::datastore::v1::Query::default(),
        };
        let namespace = Self::namespace(req.partition_id, &database)?;
        read_options_shape(req.read_options.as_ref())?;
        // An aggregation's read time is checked whole among its query's first checks.
        let read_time = past_epoch(req.read_options.as_ref())
            .and_then(|()| self.read_at(req.read_options.as_ref(), &database));
        let refused = checks::aggregation_query(&aggregation, &nested, &namespace, read_time)?;
        let mut plan = query::plan(&nested, &database, &namespace)?;
        let reading = self.read_query(req.read_options.clone(), &database, plan.kind.as_deref())?;
        if let Some(defined) = &defined {
            checks::kindless(&nested)?;
            indexes::check(&nested, &aggregated(&aggregation.aggregations), defined)?;
        }
        if let Some(refused) = refused {
            return Err(refused);
        }
        let aggregations = aggregations(&aggregation.aggregations);
        // A sum or an average reads its property: only entities that have it count.
        let key = plan.orders.pop();
        for (_, property) in &aggregations {
            if let Some(property) = property
                && !plan.row_properties.contains(property)
            {
                plan.row_properties.push(property.clone());
                plan.orders.push((Some(property.clone()), false));
            }
        }
        plan.orders.extend(key);
        let mut data = self.data();
        let read = data.read_micros();
        let stored = data.database(&database);
        let rows = query::aggregate_rows(&plan, &stored.entities);
        let mut properties = Properties::new();
        for ((alias, property), operator) in aggregations.iter().zip(&aggregation.aggregations) {
            let value = match &operator.operator {
                Some(aggregation_query::aggregation::Operator::Count(count)) => {
                    let n = i64::try_from(rows.len()).unwrap_or(i64::MAX);
                    let n = count.up_to.map_or(n, |up_to| n.min(up_to.max(0)));
                    integer(n)
                }
                Some(aggregation_query::aggregation::Operator::Sum(_)) => {
                    sum(&rows, &plan, property.as_deref().unwrap_or_default()).0
                }
                Some(aggregation_query::aggregation::Operator::Avg(_)) => {
                    let (total, count) = sum(&rows, &plan, property.as_deref().unwrap_or_default());
                    if count == 0 {
                        Value {
                            value_type: Some(ValueType::NullValue(0)),
                            ..Value::default()
                        }
                    } else {
                        let total = match total.value_type {
                            Some(ValueType::IntegerValue(n)) => n as f64,
                            Some(ValueType::DoubleValue(d)) => d,
                            _ => 0.0,
                        };
                        double(total / count as f64)
                    }
                }
                None => unreachable!("checked"),
            };
            properties.insert(alias.clone(), value);
        }
        Ok(Response::new(RunAggregationQueryResponse {
            batch: Some(
                hidane_proto::google::datastore::v1::AggregationResultBatch {
                    aggregation_results: vec![
                        hidane_proto::google::datastore::v1::AggregationResult {
                            aggregate_properties: properties,
                        },
                    ],
                    read_time: Some(timestamp(read)),
                    ..Default::default()
                },
            ),
            transaction: reading.new_transaction,
            ..RunAggregationQueryResponse::default()
        }))
    }

    async fn reserve_ids(
        &self,
        request: Request<ReserveIdsRequest>,
    ) -> Result<Response<ReserveIdsResponse>, Status> {
        let req = request.into_inner();
        let database = self.admit(req.project_id, req.database_id)?;
        self.indexes(&database)?;
        if req.keys.is_empty() {
            return Err(Status::failed_precondition(
                "List of Ids to reserve cannot be empty.",
            ));
        }
        for key in &req.keys {
            keys::check(key, &database, Rules::Write, Last::Complete)?;
        }
        Ok(Response::new(ReserveIdsResponse {}))
    }
}

/// What a request's read options are refused for before anything else of them: a consistency
/// left unspecified, a read time not after the epoch (to the microsecond), an empty transaction.
fn read_options_shape(options: Option<&ReadOptions>) -> Result<(), Status> {
    match options.and_then(|o| o.consistency_type.as_ref()) {
        Some(read_options::ConsistencyType::ReadConsistency(0)) => Err(Status::invalid_argument(
            "ReadConsistency must be specified.",
        )),
        Some(read_options::ConsistencyType::Transaction(t)) if t.is_empty() => Err(
            Status::invalid_argument("a transaction cannot be the empty string."),
        ),
        _ => Ok(()),
    }
}

/// A read time past the epoch, to the microsecond.
fn past_epoch(options: Option<&ReadOptions>) -> Result<(), Status> {
    match options.and_then(|o| o.consistency_type.as_ref()) {
        Some(read_options::ConsistencyType::ReadTime(time))
            if time.seconds < 0 || (time.seconds == 0 && time.nanos < 1_000) =>
        {
            Err(Status::invalid_argument(
                "Only timestamps past epoch are supported.",
            ))
        }
        _ => Ok(()),
    }
}

/// The checks of a mutation made once every mutation is read: a resolution strategy without a
/// detection strategy, nested arrays, the number of index values, then the base version of an
/// insert (none) and of an update (not zero).
fn late_checks(
    late: &Late,
    located: Option<&Located>,
    op: Option<Op>,
    database: &Database,
) -> Result<(), Status> {
    if late.resolution != mutation::ConflictResolutionStrategy::StrategyUnspecified as i32
        && late.required.is_none()
    {
        return Err(Status::invalid_argument(
            "cannot set a conflict resolution strategy if no detection strategy was set",
        ));
    }
    if late.nested_arrays {
        return Err(Status::invalid_argument("Nested arrays are not allowed"));
    }
    if late.index_entries > values::MAX_INDEX_ENTRIES {
        return Err(Status::invalid_argument("Too many indexed properties"));
    }
    if let (Some(required), Some(located)) = (late.required, located) {
        if op == Some(Op::Insert) && required > 0 {
            return Err(Status::invalid_argument(format!(
                "Cannot insert an entity with a base version greater than zero: {}",
                keys::proto_text(database, located)
            )));
        }
        if op == Some(Op::Update) && required == 0 {
            return Err(Status::invalid_argument(format!(
                "Cannot update an entity with a base version set to zero: {}",
                keys::proto_text(database, located)
            )));
        }
    }
    Ok(())
}

/// The properties a query's sums and averages read.
fn aggregated(list: &[aggregation_query::Aggregation]) -> Vec<String> {
    use aggregation_query::aggregation::Operator;
    list.iter()
        .filter_map(|aggregation| match &aggregation.operator {
            Some(Operator::Sum(sum)) => sum.property.as_ref().map(|p| p.name.clone()),
            Some(Operator::Avg(avg)) => avg.property.as_ref().map(|p| p.name.clone()),
            Some(Operator::Count(_)) | None => None,
        })
        .collect()
}

/// Each aggregation's alias (`property_<n>` for one without, counted from 1) and the property a
/// sum or an average reads, once `checks` has accepted them.
fn aggregations(list: &[aggregation_query::Aggregation]) -> Vec<(String, Option<String>)> {
    use aggregation_query::aggregation::Operator;
    let mut unnamed = 0;
    list.iter()
        .map(|aggregation| {
            let alias = if aggregation.alias.is_empty() {
                unnamed += 1;
                format!("property_{unnamed}")
            } else {
                aggregation.alias.clone()
            };
            let property = match &aggregation.operator {
                Some(Operator::Count(_)) | None => None,
                Some(Operator::Sum(sum)) => sum.property.as_ref().map(|p| p.name.clone()),
                Some(Operator::Avg(avg)) => avg.property.as_ref().map(|p| p.name.clone()),
            };
            (alias, property)
        })
        .collect()
}

fn integer(n: i64) -> Value {
    Value {
        value_type: Some(ValueType::IntegerValue(n)),
        ..Value::default()
    }
}

fn double(d: f64) -> Value {
    Value {
        value_type: Some(ValueType::DoubleValue(d)),
        ..Value::default()
    }
}

/// The sum of `property`'s numbers over `rows` (an integer while every one is, a double once
/// one is not or the integers overflow), and how many numbers there were. A timestamp counts as
/// its microseconds since the epoch, as the official emulator indexes it.
fn sum(rows: &[query::AggregateRow], plan: &query::Plan, property: &str) -> (Value, usize) {
    let Some(i) = plan.row_properties.iter().position(|p| p == property) else {
        return (integer(0), 0);
    };
    let mut integer_sum: Option<i64> = Some(0);
    let mut double_sum = 0.0f64;
    let mut count = 0;
    for row in rows {
        let integral = match &row.values[i].value_type {
            Some(ValueType::IntegerValue(n)) => Some(*n),
            Some(ValueType::TimestampValue(t)) => Some(
                t.seconds
                    .saturating_mul(1_000_000)
                    .saturating_add(i64::from(t.nanos / 1_000)),
            ),
            _ => None,
        };
        match (&row.values[i].value_type, integral) {
            (_, Some(n)) => {
                integer_sum = integer_sum.and_then(|s| s.checked_add(n));
                double_sum += n as f64;
                count += 1;
            }
            (Some(ValueType::DoubleValue(d)), _) => {
                integer_sum = None;
                double_sum += d;
                count += 1;
            }
            _ => {}
        }
    }
    (
        integer_sum.map_or_else(|| double(double_sum), integer),
        count,
    )
}

/// Applies a commit's mutations, all or none, and returns their results and the commit's time.
fn apply(
    data: &mut Data,
    database: &Database,
    checked: Vec<Checked>,
    transactional: bool,
) -> Result<(Vec<MutationResult>, i64), Status> {
    let micros = data.next_micros();
    let stored = data.database(database);
    // Each mutation's late checks, then the checks against the entity as the mutations before
    // it left it, then the mutation itself (`tests/fixtures/datastore_commit_checks.json`). A
    // transaction writes everything or nothing; a non-transactional commit keeps the mutations
    // before one that fails, as the official emulator does.
    let mut working: BTreeMap<Located, Option<Stored>> = BTreeMap::new();
    let mut written: Vec<(Located, bool, bool)> = Vec::with_capacity(checked.len());
    for mutation in checked {
        let (late, located, op) = match &mutation {
            Checked::Write { op, key, late, .. } => (late, key.complete(), Some(*op)),
            Checked::Delete(located, late) => (late, Some(located.clone()), None),
        };
        late_checks(late, located.as_ref(), op, database)?;
        let current = located.as_ref().and_then(|l| match working.get(l) {
            Some(entity) => entity.clone(),
            None => stored.entities.get(l).cloned(),
        });
        if late.resolution == mutation::ConflictResolutionStrategy::Fail as i32
            && let Some(required) = late.required
        {
            let version = current.as_ref().map_or(0, |e| e.version);
            if version != required {
                return Err(Status::failed_precondition(format!(
                    "the stored version ({version}) does not match the required base version ({required})"
                )));
            }
        }
        if let (Some(op), Some(located)) = (op, &located) {
            if op == Op::Insert && current.is_some() {
                return Err(Status::already_exists(format!(
                    "entity already exists: {}",
                    keys::entity_ref(database, located)
                )));
            }
            if op == Op::Update && current.is_none() {
                return Err(Status::not_found(format!(
                    "no entity to update: {}",
                    keys::proto_text(database, located)
                )));
            }
        }
        if late.size > values::MAX_ENTITY_BYTES {
            return Err(Status::invalid_argument(format!(
                "maximum entity size is {} bytes",
                values::MAX_ENTITY_BYTES
            )));
        }
        let (located, entity, allocated) = match mutation {
            Checked::Delete(located, _) => (located, None, false),
            Checked::Write {
                key,
                properties,
                mask,
                ..
            } => {
                let (located, allocated) = match key.complete() {
                    Some(located) => (located, false),
                    None => {
                        stored.allocated += 1;
                        (key.with_id(scattered(stored.allocated)), true)
                    }
                };
                let properties = match (mask, &current) {
                    (Some(paths), Some(previous)) => {
                        let mut merged = previous.properties.clone();
                        for path in &paths {
                            match properties.get(path) {
                                Some(value) => {
                                    merged.insert(path.clone(), value.clone());
                                }
                                None => {
                                    merged.remove(path);
                                }
                            }
                        }
                        merged
                    }
                    (Some(paths), None) => properties
                        .into_iter()
                        .filter(|(name, _)| paths.contains(name))
                        .collect(),
                    (None, _) => properties,
                };
                // A write that changes nothing keeps the entity's version and times.
                let entity = match &current {
                    Some(previous) if previous.properties == properties => previous.clone(),
                    _ => Stored {
                        properties,
                        version: micros,
                        created: current.as_ref().map_or(micros, |p| p.created),
                        updated: micros,
                    },
                };
                (located, Some(entity), allocated)
            }
        };
        let deleted = entity.is_none();
        if transactional {
            working.insert(located.clone(), entity);
        } else {
            stored.put(located.clone(), entity, micros);
        }
        written.push((located, allocated, deleted));
    }
    for (located, entity) in working {
        stored.put(located, entity, micros);
    }
    // Each result tells of its entity as the commit left it.
    let results = written
        .into_iter()
        .map(|(located, allocated, deleted)| {
            let key = allocated.then(|| database.key(&located));
            match stored
                .entities
                .get(&located)
                .filter(|_| !deleted || transactional)
            {
                Some(entity) => MutationResult {
                    key,
                    version: entity.version,
                    create_time: Some(timestamp(entity.created)),
                    update_time: Some(timestamp(entity.updated)),
                    ..MutationResult::default()
                },
                None => MutationResult {
                    key,
                    version: micros,
                    ..MutationResult::default()
                },
            }
        })
        .collect();
    Ok((results, micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The official emulator's first IDs (`tests/fixtures/datastore_entities.json`).
    #[test]
    fn ids_are_scattered_as_the_official_emulator_scatters_them() {
        assert_eq!(scattered(1), 5_629_499_534_213_120);
        assert_eq!(scattered(2), 5_066_549_580_791_808);
        assert_eq!(scattered(3), 6_192_449_487_634_432);
        assert_eq!(scattered(4), 4_785_074_604_081_152);
        assert_eq!(scattered(12), 4_925_812_092_436_480);
    }
}
