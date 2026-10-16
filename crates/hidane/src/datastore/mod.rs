//! Datastore mode (#81, [ADR 0009](../../../../docs/adr/0009-datastore-mode.md)): with
//! `--database-mode datastore-mode`, hidane serves `google.datastore.v1.Datastore` over gRPC and
//! REST in place of the Firestore API, which it refuses as the official emulator does
//! ([`FIRESTORE_REFUSED`]).
//!
//! Entities live here, apart from the Firestore engine's documents: keys and values are
//! Datastore's ([`keys`], [`values`]). Transactions are the Firestore engine's
//! ([`Transactions`]): the official emulator locks, waits and expires them alike in both modes,
//! with the same messages, a key's lock kept under [`keys::lock_path`]. What is served so far:
//! `Lookup`, `Commit`, `BeginTransaction`, `Rollback`, `AllocateIds` and `ReserveIds`
//! (`tests/fixtures/datastore_entities.json`), and structured `RunQuery` and
//! `RunAggregationQuery` ([`query`], `tests/fixtures/datastore_queries.json`), and GQL
//! ([`gql`], `tests/fixtures/datastore_gql.json`).

mod gql;
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

/// The Datastore service, sharing its state between gRPC and REST.
#[derive(Clone, Default)]
pub struct DatastoreService {
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    data: Mutex<Data>,
    transactions: Transactions,
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
    /// The last ID allocated: one counter per database, whatever the kind or the namespace.
    allocated: u64,
    /// When the database was first used, in microseconds: reads before it are too old.
    created: i64,
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

    /// Every entity of every database, and every open transaction (`POST /reset`).
    pub fn reset(&self) {
        *self.data() = Data::default();
        self.state.transactions.clear();
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
        if !partition.project_id.is_empty() && partition.project_id != database.project {
            return Err(Status::invalid_argument(format!(
                "mismatched databases within request: <unknown!>~{} vs. <unknown!>~{}",
                database.project, partition.project_id
            )));
        }
        Ok(partition.namespace_id)
    }

    /// A query's read options: a read time is checked; a transaction locks `kind` (every kind
    /// without one, as a key range); eventual consistency reads as strong.
    fn read_query(
        &self,
        options: Option<ReadOptions>,
        database: &Database,
        kind: Option<&str>,
    ) -> Result<Reading, Status> {
        let mut reading = Reading {
            new_transaction: Vec::new(),
        };
        let lock = |transaction: &[u8]| {
            self.state.transactions.read(
                &database.name(),
                transaction,
                &[],
                Some(kind.unwrap_or("")),
            )
        };
        match options.and_then(|o| o.consistency_type) {
            None | Some(read_options::ConsistencyType::ReadConsistency(_)) => {}
            Some(read_options::ConsistencyType::ReadTime(time)) => {
                self.check_read_time(database, &time)?;
            }
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
    },
    Delete(Located),
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
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
        let mut wanted: Vec<Located> = Vec::with_capacity(req.keys.len());
        for key in &req.keys {
            let located = keys::check(key, &database, Rules::Read, Last::Complete)?
                .complete()
                .expect("checked complete");
            if !wanted.contains(&located) {
                wanted.push(located);
            }
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
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
        let transaction = self.begin(&database, req.transaction_options.as_ref())?;
        Ok(Response::new(BeginTransactionResponse { transaction }))
    }

    async fn commit(
        &self,
        request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        let req = request.into_inner();
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
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
        if let Commit::Transactional(t) = &how {
            self.state
                .transactions
                .check_commit(&name, t, !req.mutations.is_empty())?;
        }
        // Every mutation is checked before anything is written.
        let mut checked = Vec::with_capacity(req.mutations.len());
        for mutation in req.mutations {
            let mask = mutation.property_mask.map(|m| m.paths);
            checked.push(match mutation.operation {
                None => return Err(Status::invalid_argument("Mutation is missing operation.")),
                Some(mutation::Operation::Delete(key)) => Checked::Delete(
                    keys::check(&key, &database, Rules::Write, Last::Complete)?
                        .complete()
                        .expect("checked complete"),
                ),
                Some(operation) => {
                    let (op, entity) = match operation {
                        mutation::Operation::Insert(e) => (Op::Insert, e),
                        mutation::Operation::Update(e) => (Op::Update, e),
                        mutation::Operation::Upsert(e) => (Op::Upsert, e),
                        mutation::Operation::Delete(_) => unreachable!("handled above"),
                    };
                    let last = if op == Op::Update {
                        Last::Complete
                    } else {
                        Last::Either
                    };
                    let key = keys::check(
                        &entity.key.clone().unwrap_or_default(),
                        &database,
                        Rules::Write,
                        last,
                    )?;
                    values::check_properties(&entity.properties, &database)?;
                    let mut properties = entity.properties;
                    values::normalize(&mut properties, &database);
                    Checked::Write {
                        op,
                        key,
                        properties,
                        mask,
                    }
                }
            });
        }
        let complete: Vec<Located> = checked
            .iter()
            .filter_map(|c| match c {
                Checked::Write { key, .. } => key.complete(),
                Checked::Delete(located) => Some(located.clone()),
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
                apply(&mut data, &database, checked)
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
        if req.transaction.is_empty() {
            return Err(Status::invalid_argument("transaction required"));
        }
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
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
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
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
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
        let namespace = Self::namespace(req.partition_id, &database)?;
        let mut echo = None;
        let query = match req.query_type {
            Some(run_query_request::QueryType::Query(query)) => query,
            Some(run_query_request::QueryType::GqlQuery(text)) => match gql::read(&text)? {
                gql::Read::Entities(query) => {
                    echo = Some(query.clone());
                    query
                }
                gql::Read::Aggregation(_) => {
                    return Err(Status::invalid_argument(
                        "Expecting ENTITY_QUERY, but got AGGREGATION_QUERY. Use Datastore.RunAggregationQuery API instead for such queries.",
                    ));
                }
            },
            None => {
                return Err(Status::invalid_argument(
                    "one of fields Query.query and Query.gql_query must be set",
                ));
            }
        };
        let plan = query::plan(&query, &database, &namespace)?;
        let reading = self.read_query(req.read_options, &database, plan.kind.as_deref())?;
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
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
        let namespace = Self::namespace(req.partition_id, &database)?;
        let aggregation = match req.query_type {
            Some(run_aggregation_query_request::QueryType::AggregationQuery(a)) => a,
            Some(run_aggregation_query_request::QueryType::GqlQuery(text)) => {
                match gql::read(&text)? {
                    gql::Read::Aggregation(aggregation) => aggregation,
                    gql::Read::Entities(_) => {
                        return Err(Status::invalid_argument(
                            "Expecting AGGREGATION_QUERY, but got ENTITY_QUERY. Use Datastore.RunQuery API instead for such queries.",
                        ));
                    }
                }
            }
            None => {
                return Err(Status::invalid_argument(
                    "one of fields Query.query and Query.gql_query must be set",
                ));
            }
        };
        let aggregations = aggregations(&aggregation.aggregations)?;
        let nested = match aggregation.query_type {
            Some(aggregation_query::QueryType::NestedQuery(query)) => query,
            None => hidane_proto::google::datastore::v1::Query::default(),
        };
        let mut plan = query::plan(&nested, &database, &namespace)?;
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
        let reading = self.read_query(req.read_options, &database, plan.kind.as_deref())?;
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
        let database = Database {
            project: req.project_id,
            database: req.database_id,
        };
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

/// The most aggregations a query may have.
const MAX_AGGREGATIONS: usize = 5;

/// Each aggregation's alias (`property_<n>` for one without, counted from 1) and the property a
/// sum or an average reads.
fn aggregations(
    list: &[aggregation_query::Aggregation],
) -> Result<Vec<(String, Option<String>)>, Status> {
    use aggregation_query::aggregation::Operator;
    if list.is_empty() {
        return Err(Status::invalid_argument("Aggregations can not be empty."));
    }
    if list.len() > MAX_AGGREGATIONS {
        return Err(Status::invalid_argument(format!(
            "The maximum number of aggregations allowed in an aggregation query is {MAX_AGGREGATIONS}. Received: {}",
            list.len()
        )));
    }
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut unnamed = 0;
    for aggregation in list {
        let alias = if aggregation.alias.is_empty() {
            unnamed += 1;
            format!("property_{unnamed}")
        } else {
            aggregation.alias.clone()
        };
        if alias.len() >= 4 && alias.starts_with("__") && alias.ends_with("__") {
            return Err(Status::invalid_argument(format!(
                "The property.name \"{alias}\" is reserved."
            )));
        }
        if out.iter().any(|(a, _)| *a == alias) {
            return Err(Status::invalid_argument(format!(
                "Aggregation aliases contain duplicate alias: {alias}."
            )));
        }
        let property = match &aggregation.operator {
            Some(Operator::Count(_)) | None => None,
            Some(Operator::Sum(sum)) => sum.property.as_ref().map(|p| p.name.clone()),
            Some(Operator::Avg(avg)) => avg.property.as_ref().map(|p| p.name.clone()),
        };
        out.push((alias, property));
    }
    Ok(out)
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
/// one is not or the integers overflow), and how many numbers there were.
fn sum(rows: &[query::AggregateRow], plan: &query::Plan, property: &str) -> (Value, usize) {
    let Some(i) = plan.row_properties.iter().position(|p| p == property) else {
        return (integer(0), 0);
    };
    let mut integer_sum: Option<i64> = Some(0);
    let mut double_sum = 0.0f64;
    let mut count = 0;
    for row in rows {
        match &row.values[i].value_type {
            Some(ValueType::IntegerValue(n)) => {
                integer_sum = integer_sum.and_then(|s| s.checked_add(*n));
                double_sum += *n as f64;
                count += 1;
            }
            Some(ValueType::DoubleValue(d)) => {
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
) -> Result<(Vec<MutationResult>, i64), Status> {
    // The checks against what is stored come first, so a failing mutation writes nothing.
    {
        let stored = data.database(database);
        for mutation in &checked {
            if let Checked::Write { op, key, .. } = mutation
                && let Some(located) = key.complete()
            {
                let exists = stored.entities.contains_key(&located);
                if *op == Op::Insert && exists {
                    return Err(Status::already_exists(format!(
                        "entity already exists: {}",
                        keys::entity_ref(database, &located)
                    )));
                }
                if *op == Op::Update && !exists {
                    return Err(Status::not_found(format!(
                        "no entity to update: {}",
                        keys::proto_text(database, &located)
                    )));
                }
            }
        }
    }
    let micros = data.next_micros();
    let stored = data.database(database);
    let mut results = Vec::with_capacity(checked.len());
    for mutation in checked {
        match mutation {
            Checked::Delete(located) => {
                stored.entities.remove(&located);
                results.push(MutationResult {
                    version: micros,
                    ..MutationResult::default()
                });
            }
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
                let previous = stored.entities.get(&located);
                let created = previous.map_or(micros, |p| p.created);
                let properties = match (mask, previous) {
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
                results.push(MutationResult {
                    key: allocated.then(|| database.key(&located)),
                    version: micros,
                    create_time: Some(timestamp(created)),
                    update_time: Some(timestamp(micros)),
                    ..MutationResult::default()
                });
                stored.entities.insert(
                    located,
                    Stored {
                        properties,
                        version: micros,
                        created,
                        updated: micros,
                    },
                );
            }
        }
    }
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
