//! Transactions, with the official emulator's lock model (recorded in
//! `tests/fixtures/transactions.json` by `tools/oracle/transactions.py`):
//!
//! - A read-write transaction takes a shared lock on every document it reads, found or not.
//!   ListDocuments (and queries, #21) lock every collection with the listed collection ID,
//!   whatever its parent.
//! - A write, in a transaction or not, waits until no *other* transaction holds a lock covering
//!   its document. After [`LOCK_TIMEOUT`] it fails with `ABORTED Transaction lock timeout.`, and
//!   a transaction that times out this way ends.
//! - A transaction ends, releasing its locks, when it commits (successfully or not, except for
//!   an invalid write, which is rejected before anything else happens), rolls back, times out
//!   waiting, or stays unused for [`IDLE_TIMEOUT`].
//! - A read-only transaction takes no locks and reads the snapshot of its start.
//!
//! The waiting is real: two transactions that read and then write the same document block each
//! other until the first one gives up, as on the official emulator.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use hidane_core::{path::ResourcePath, store::ReadTime};
use tokio::{sync::watch, time::Instant};
use tonic::{Code, Status};

/// How long a write waits for other transactions' locks (2 s on the official emulator).
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an unused transaction stays open (60 s since its last request, as measured).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

const LOCK_TIMEOUT_MESSAGE: &str = "Transaction lock timeout.";
const EXPIRED: &str = "The referenced transaction has expired or is no longer valid.";
const MALFORMED: &str = "Invalid transaction.";
/// For well-formed IDs that were never issued. The official emulator answers `UNKNOWN` with
/// no message for reads and commits, and this for rollbacks (docs/parity-exceptions.md).
const UNKNOWN: &str = "Transaction is invalid or expired.";
const READ_ONLY: &str = "Cannot modify entities in a read-only transaction.";

/// How a transaction reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Reads see the latest data and lock what they read.
    ReadWrite,
    /// Reads see the database as of this time and lock nothing.
    ReadOnly(ReadTime),
}

/// The open transactions of every database, and the locks they hold.
///
/// Each database has its own mutex, so databases (often one per test file) never wait for each
/// other. Within a database, lock checks and commits take turns.
pub struct Transactions {
    databases: Mutex<HashMap<String, Arc<Mutex<Database>>>>,
    /// Bumped whenever locks are released, to wake waiting writes.
    released: watch::Sender<()>,
    lock_timeout: Duration,
    idle_timeout: Duration,
}

#[derive(Default)]
struct Database {
    /// The last ID handed out. IDs count from 1 per database, like the official emulator's,
    /// and keep counting across a reset so that an old ID never names a new transaction.
    issued: u64,
    open: HashMap<u64, Open>,
}

struct Open {
    mode: Mode,
    last_used: Instant,
    documents: HashSet<ResourcePath>,
    collection_ids: HashSet<String>,
}

impl Open {
    fn covers(&self, document: &ResourcePath) -> bool {
        self.documents.contains(document)
            || document
                .collection_id()
                .is_some_and(|id| self.collection_ids.contains(id))
    }
}

impl Database {
    fn forget_idle(&mut self, now: Instant, idle_timeout: Duration) {
        self.open
            .retain(|_, txn| now.duration_since(txn.last_used) < idle_timeout);
    }

    fn was_issued(&self, id: u64) -> bool {
        (1..=self.issued).contains(&id)
    }

    /// The open transaction `id`, marked as used now.
    fn get(&mut self, id: u64, now: Instant) -> Result<&mut Open, Status> {
        if !self.open.contains_key(&id) {
            return Err(if self.was_issued(id) {
                Status::aborted(EXPIRED)
            } else {
                Status::invalid_argument(UNKNOWN)
            });
        }
        let txn = self.open.get_mut(&id).expect("checked above");
        txn.last_used = now;
        Ok(txn)
    }

    /// When the first transaction other than `owner` that locks one of `targets` expires, or
    /// `None` when nothing blocks the write.
    fn blocked_until(
        &self,
        owner: Option<u64>,
        targets: &[ResourcePath],
        idle_timeout: Duration,
    ) -> Option<Instant> {
        self.open
            .iter()
            .filter(|(id, txn)| Some(**id) != owner && targets.iter().any(|t| txn.covers(t)))
            .map(|(_, txn)| txn.last_used + idle_timeout)
            .min()
    }
}

impl Default for Transactions {
    fn default() -> Self {
        Self::with_timeouts(LOCK_TIMEOUT, IDLE_TIMEOUT)
    }
}

impl Transactions {
    pub fn with_timeouts(lock_timeout: Duration, idle_timeout: Duration) -> Self {
        Self {
            databases: Mutex::default(),
            released: watch::Sender::new(()),
            lock_timeout,
            idle_timeout,
        }
    }

    fn database(&self, database: &str) -> Arc<Mutex<Database>> {
        let mut databases = self
            .databases
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(db) = databases.get(database) {
            return Arc::clone(db);
        }
        Arc::clone(databases.entry(database.to_owned()).or_default())
    }

    /// Locks `db`, forgetting the transactions that have been idle too long.
    fn lock<'a>(&self, db: &'a Mutex<Database>, now: Instant) -> MutexGuard<'a, Database> {
        let mut db = db.lock().unwrap_or_else(PoisonError::into_inner);
        db.forget_idle(now, self.idle_timeout);
        db
    }

    /// Opens a transaction and returns its ID.
    pub fn begin(&self, database: &str, mode: Mode) -> Vec<u8> {
        let now = Instant::now();
        let shared = self.database(database);
        let mut db = self.lock(&shared, now);
        db.issued += 1;
        let id = db.issued;
        db.open.insert(
            id,
            Open {
                mode,
                last_used: now,
                documents: HashSet::new(),
                collection_ids: HashSet::new(),
            },
        );
        encode(id)
    }

    /// Checks the `retry_transaction` of a new read-write transaction. The previous attempt
    /// has usually ended already, which is fine; the ID only has to be one of ours.
    pub fn check_retry(&self, database: &str, transaction: &[u8]) -> Result<(), Status> {
        let id = decode(transaction)?;
        let shared = self.database(database);
        if self.lock(&shared, Instant::now()).was_issued(id) {
            Ok(())
        } else {
            Err(Status::invalid_argument(UNKNOWN))
        }
    }

    /// Registers a read of `documents`, or of every collection with ID `collection_id`, by
    /// `transaction`, and returns how it reads. A read-write transaction locks what it reads
    /// before reading it, so the read is consistent with the transaction's commit.
    pub fn read(
        &self,
        database: &str,
        transaction: &[u8],
        documents: &[ResourcePath],
        collection_id: Option<&str>,
    ) -> Result<Mode, Status> {
        let id = decode(transaction)?;
        let now = Instant::now();
        let shared = self.database(database);
        let mut db = self.lock(&shared, now);
        let txn = db.get(id, now)?;
        if txn.mode == Mode::ReadWrite {
            txn.documents.extend(documents.iter().cloned());
            txn.collection_ids.extend(collection_id.map(str::to_owned));
        }
        Ok(txn.mode)
    }

    /// Ends `transaction`. Rolling back a transaction that already ended succeeds.
    pub fn rollback(&self, database: &str, transaction: &[u8]) -> Result<(), Status> {
        let id = decode(transaction)?;
        let shared = self.database(database);
        let mut db = self.lock(&shared, Instant::now());
        if db.open.remove(&id).is_some() {
            drop(db);
            self.released.send_replace(());
            Ok(())
        } else if db.was_issued(id) {
            Ok(())
        } else {
            Err(Status::invalid_argument(UNKNOWN))
        }
    }

    /// Runs `commit`, which writes `targets`, once no transaction other than `transaction`
    /// holds a lock on them. Nothing else in the database can lock or commit while `commit`
    /// runs.
    ///
    /// A transaction ends with its commit, unless `commit` rejects an invalid write
    /// (`INVALID_ARGUMENT`): the official emulator checks those first and leaves the
    /// transaction open.
    pub async fn commit<T>(
        &self,
        database: &str,
        transaction: Option<&[u8]>,
        targets: &[ResourcePath],
        commit: impl FnOnce() -> Result<T, Status>,
    ) -> Result<T, Status> {
        let owner = transaction.map(decode).transpose()?;
        let shared = self.database(database);
        let deadline = Instant::now() + self.lock_timeout;
        let mut commit = Some(commit);
        loop {
            // Subscribe before looking, so a release between the look and the wait wakes us.
            let mut released = self.released.subscribe();
            let wake = {
                let now = Instant::now();
                let mut db = self.lock(&shared, now);
                if let Some(id) = owner {
                    let txn = db.get(id, now)?;
                    if matches!(txn.mode, Mode::ReadOnly(_)) && !targets.is_empty() {
                        return Err(Status::invalid_argument(READ_ONLY));
                    }
                }
                match db.blocked_until(owner, targets, self.idle_timeout) {
                    None => {
                        let result = commit.take().expect("commit runs once")();
                        let rejected =
                            matches!(&result, Err(s) if s.code() == Code::InvalidArgument);
                        if let Some(id) = owner
                            && !rejected
                        {
                            db.open.remove(&id);
                            drop(db);
                            self.released.send_replace(());
                        }
                        return result;
                    }
                    Some(expiry) if now < deadline => expiry.min(deadline),
                    Some(_) => {
                        if let Some(id) = owner {
                            db.open.remove(&id);
                            drop(db);
                            self.released.send_replace(());
                        }
                        return Err(Status::aborted(LOCK_TIMEOUT_MESSAGE));
                    }
                }
            };
            tokio::select! {
                _ = released.changed() => {}
                () = tokio::time::sleep_until(wake) => {}
            }
        }
    }

    /// Forgets every open transaction (`POST /reset`).
    pub fn clear(&self) {
        let databases: Vec<_> = self
            .databases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for db in databases {
            db.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .open
                .clear();
        }
        self.released.send_replace(());
    }
}

/// The official emulator's format: a protobuf message whose field 2 (fixed64) is the counter.
fn encode(id: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(0x11);
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes
}

fn decode(transaction: &[u8]) -> Result<u64, Status> {
    match transaction {
        [0x11, id @ ..] => id
            .try_into()
            .map(u64::from_le_bytes)
            .map_err(|_| Status::invalid_argument(MALFORMED)),
        _ => Err(Status::invalid_argument(MALFORMED)),
    }
}

#[cfg(test)]
mod tests;
