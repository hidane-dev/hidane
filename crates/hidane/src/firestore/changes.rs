//! The change feed: every commit of a database, in commit order, for Listen streams.
//!
//! Commits are published while the database's commit lock is held (inside
//! `Transactions::commit`), so they arrive in commit time order. Alongside each database's
//! broadcast channel, the feed records the commit time up to which everything has been sent.
//! A listener subscribes first, then reads that time T and the store at T: commits up to T are
//! in its snapshot (it skips them when they arrive), later ones come through the channel, and
//! none is missed or applied twice.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_core::store::{Commit, ReadTime};
use tokio::sync::broadcast;

/// How many commits a listener may fall behind before it has to start over.
const CAPACITY: usize = 4096;

pub struct ChangeFeed {
    databases: Mutex<HashMap<String, Arc<Feed>>>,
    /// Data before this time may differ from what this process (or the store since its last
    /// reset) holds: resume tokens from before it cannot be trusted.
    history_start: Mutex<ReadTime>,
}

impl Default for ChangeFeed {
    fn default() -> Self {
        Self {
            databases: Mutex::default(),
            history_start: Mutex::new(now()),
        }
    }
}

fn now() -> ReadTime {
    ReadTime(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX)),
    )
}

struct Feed {
    sender: broadcast::Sender<Arc<Commit>>,
    /// Every commit up to this time has been sent.
    published: Mutex<ReadTime>,
}

impl ChangeFeed {
    fn feed(&self, database: &str) -> Arc<Feed> {
        let mut databases = self
            .databases
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(feed) = databases.get(database) {
            return Arc::clone(feed);
        }
        // Every commit publishes, so a feed made by a subscriber belongs to a database nothing
        // was committed to yet: everything up to now is "published". Not time 0, which the
        // web SDK treats as "no snapshot" and never shows. The first commit will be later.
        let feed = Arc::new(Feed {
            sender: broadcast::channel(CAPACITY).0,
            published: Mutex::new(ReadTime(now().0 - 1)),
        });
        databases.insert(database.to_owned(), Arc::clone(&feed));
        feed
    }

    /// Publishes `commit`. Callers hold the database's commit lock.
    pub fn publish(&self, database: &str, commit: Commit) {
        let feed = self.feed(database);
        let mut published = feed
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let commit_time = commit.commit_time;
        if !commit.changes.is_empty() {
            // No receiver is fine: nobody listens.
            let _ = feed.sender.send(Arc::new(commit));
        }
        *published = commit_time;
    }

    pub fn subscribe(&self, database: &str) -> broadcast::Receiver<Arc<Commit>> {
        self.feed(database).sender.subscribe()
    }

    /// The store was cleared (`POST /reset`): history starts again now.
    pub fn reset(&self) {
        *self
            .history_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = now();
    }

    /// Resume points before this time are not trusted.
    pub fn history_start(&self) -> ReadTime {
        *self
            .history_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The time up to which every commit has been sent to subscribers.
    pub fn published(&self, database: &str) -> ReadTime {
        *self
            .feed(database)
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
