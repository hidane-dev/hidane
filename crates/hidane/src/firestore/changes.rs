//! The change feed: every commit and clear of a database, in time order, for Listen streams.
//!
//! Events are published while the database's commit lock is held (inside
//! `Transactions::commit`), so they arrive in time order. Alongside each database's broadcast
//! channel, the feed records the time up to which everything has been sent. A listener
//! subscribes first, then reads that time T and the store at T: events up to T are in its
//! snapshot (it skips them when they arrive), later ones come through the channel, and none is
//! missed or applied twice.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_core::store::{Commit, ReadTime};
use tokio::sync::broadcast;

/// How many events a listener may fall behind before it has to start over.
const CAPACITY: usize = 4096;

#[derive(Debug, Clone)]
pub enum Event {
    Commit(Arc<Commit>),
    /// Every document of the database was dropped, with its history, at this time.
    Cleared(ReadTime),
}

impl Event {
    pub fn time(&self) -> ReadTime {
        match self {
            Self::Commit(commit) => commit.commit_time,
            Self::Cleared(at) => *at,
        }
    }
}

#[derive(Default)]
pub struct ChangeFeed {
    databases: Mutex<HashMap<String, Arc<Feed>>>,
}

struct Feed {
    sender: broadcast::Sender<Event>,
    /// Every event up to this time has been sent.
    published: Mutex<ReadTime>,
    /// The store may not hold what the database looked like before this time (the process
    /// started, or the database was cleared): resume tokens from before it are not trusted.
    history_start: Mutex<ReadTime>,
}

fn now() -> ReadTime {
    ReadTime(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX)),
    )
}

fn lock<T: Copy>(value: &Mutex<T>) -> T {
    *value.lock().unwrap_or_else(PoisonError::into_inner)
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
        let now = now();
        let feed = Arc::new(Feed {
            sender: broadcast::channel(CAPACITY).0,
            published: Mutex::new(ReadTime(now.0 - 1)),
            history_start: Mutex::new(now),
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
            let _ = feed.sender.send(Event::Commit(Arc::new(commit)));
        }
        *published = commit_time;
        // This process made the commit, so history reaches back to it at least (a feed created
        // by this very call starts a little after the commit time was taken).
        let mut history_start = feed
            .history_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if commit_time < *history_start {
            *history_start = commit_time;
        }
    }

    /// Publishes that `database` was cleared at `at`. Callers hold its commit lock.
    pub fn publish_cleared(&self, database: &str, at: ReadTime) {
        let feed = self.feed(database);
        let mut published = feed
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = feed.sender.send(Event::Cleared(at));
        *feed
            .history_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = at;
        *published = at;
    }

    pub fn subscribe(&self, database: &str) -> broadcast::Receiver<Event> {
        self.feed(database).sender.subscribe()
    }

    /// The time up to which every event has been sent to subscribers.
    pub fn published(&self, database: &str) -> ReadTime {
        lock(&self.feed(database).published)
    }

    /// Resume points before this time are not trusted.
    pub fn history_start(&self, database: &str) -> ReadTime {
        lock(&self.feed(database).history_start)
    }
}
