use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use hidane_proto::google::firestore::v1::{Value, value::ValueType};

use super::MemoryStore;
use crate::{
    path::ResourcePath,
    store::{ReadTime, Store, StoreError, StoredDocument},
};

const DB: &str = "projects/demo/databases/(default)";

fn path(p: &str) -> ResourcePath {
    ResourcePath::parse(p).unwrap()
}

fn int(i: i64) -> Value {
    Value {
        value_type: Some(ValueType::IntegerValue(i)),
    }
}

fn fields(n: i64) -> BTreeMap<String, Value> {
    [("n".to_owned(), int(n))].into()
}

fn n_of(doc: &StoredDocument) -> i64 {
    match doc.fields()["n"].value_type {
        Some(ValueType::IntegerValue(n)) => n,
        _ => panic!("no n"),
    }
}

/// A store whose clock the test moves by hand.
fn store_with_clock(start: i64) -> (MemoryStore, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(start));
    let clock = now.clone();
    (
        MemoryStore::with_clock(move || clock.load(Ordering::SeqCst)),
        now,
    )
}

fn set(store: &MemoryStore, p: &str, n: i64) -> ReadTime {
    store
        .commit(DB, &mut |batch| {
            batch.set(&path(p), fields(n));
            Ok(())
        })
        .unwrap()
        .commit_time
}

fn delete(store: &MemoryStore, p: &str) -> ReadTime {
    store
        .commit(DB, &mut |batch| {
            batch.delete(&path(p));
            Ok(())
        })
        .unwrap()
        .commit_time
}

fn collect(
    scan: impl FnOnce(&mut dyn FnMut(&Arc<StoredDocument>) -> ControlFlow<()>),
) -> Vec<String> {
    let mut names = Vec::new();
    scan(&mut |doc| {
        names.push(doc.path.to_string());
        ControlFlow::Continue(())
    });
    names
}

#[test]
fn reads_see_the_version_at_their_read_time() {
    let (store, now) = store_with_clock(1_000);
    let t1 = set(&store, "users/alice", 1);
    now.store(2_000, Ordering::SeqCst);
    let t2 = set(&store, "users/alice", 2);
    now.store(3_000, Ordering::SeqCst);
    let t3 = delete(&store, "users/alice");

    let alice = path("users/alice");
    assert_eq!(store.get(DB, &alice, ReadTime(t1.0 - 1)), None);
    let v1 = store.get(DB, &alice, t1).unwrap();
    let v2 = store.get(DB, &alice, ReadTime(t3.0 - 1)).unwrap();
    assert_eq!((n_of(&v1), n_of(&v2)), (1, 2));
    assert_eq!(store.get(DB, &alice, t3), None);
    assert_eq!(store.get(DB, &alice, ReadTime::MAX), None);

    // create_time survives updates; update_time is the commit time.
    assert_eq!(v1.create_time, t1.to_timestamp());
    assert_eq!(v2.create_time, t1.to_timestamp());
    assert_eq!(v2.update_time, t2.to_timestamp());
}

#[test]
fn commits_report_what_changed() {
    let store = MemoryStore::new();
    set(&store, "users/alice", 1);
    let commit = store
        .commit(DB, &mut |batch| {
            batch.set(&path("users/alice"), fields(2));
            batch.set(&path("users/bob"), fields(1));
            batch.delete(&path("users/carol")); // never existed: not a change
            Ok(())
        })
        .unwrap();
    let summary: Vec<_> = commit
        .changes
        .iter()
        .map(|c| {
            (
                c.path.to_string(),
                c.before.as_deref().map(n_of),
                c.after.as_deref().map(n_of),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("users/alice".to_owned(), Some(1), Some(2)),
            ("users/bob".to_owned(), None, Some(1)),
        ]
    );
}

#[test]
fn batch_reads_its_own_writes() {
    let store = MemoryStore::new();
    set(&store, "users/alice", 1);
    store
        .commit(DB, &mut |batch| {
            assert_eq!(n_of(&batch.get(&path("users/alice")).unwrap()), 1);
            batch.set(&path("users/alice"), fields(2));
            assert_eq!(n_of(&batch.get(&path("users/alice")).unwrap()), 2);
            batch.delete(&path("users/alice"));
            assert!(batch.get(&path("users/alice")).is_none());
            batch.set(&path("users/alice"), fields(3));
            Ok(())
        })
        .unwrap();
    let alice = store.get(DB, &path("users/alice"), ReadTime::MAX).unwrap();
    assert_eq!(n_of(&alice), 3);
}

#[test]
fn a_failed_commit_applies_nothing() {
    let store = MemoryStore::new();
    set(&store, "users/alice", 1);
    let err = store
        .commit(DB, &mut |batch| {
            batch.set(&path("users/alice"), fields(99));
            batch.set(&path("users/bob"), fields(99));
            Err(StoreError::FailedPrecondition("no".into()))
        })
        .unwrap_err();
    assert_eq!(err, StoreError::FailedPrecondition("no".into()));
    let alice = store.get(DB, &path("users/alice"), ReadTime::MAX).unwrap();
    assert_eq!(n_of(&alice), 1);
    assert!(store.get(DB, &path("users/bob"), ReadTime::MAX).is_none());
}

#[test]
fn commit_times_increase_even_with_a_frozen_clock() {
    let (store, _) = store_with_clock(5_000);
    let times: Vec<i64> = (0..5).map(|i| set(&store, "c/d", i).0).collect();
    assert_eq!(times, [5_000, 5_001, 5_002, 5_003, 5_004]);
    // The latest read time sees every commit even though the clock is behind them.
    assert_eq!(store.latest_read_time(DB), ReadTime(5_004));
    let latest = store
        .get(DB, &path("c/d"), store.latest_read_time(DB))
        .unwrap();
    assert_eq!(n_of(&latest), 4);
}

#[test]
fn collection_scans_skip_subcollections_and_follow_name_order() {
    let store = MemoryStore::new();
    for p in [
        "c/b",
        "c/a/sub/x",
        "c/a",
        "c/__id10__",
        "c/__id5__",
        "c/\u{1f600}",
        "c/\u{ff5e}",
        "c/aa/deeper/y",
        "c/aa",
        "c-b/x",
        "other/z",
    ] {
        set(&store, p, 0);
    }
    let names = collect(|visit| store.scan_collection(DB, &path("c"), ReadTime::MAX, visit));
    assert_eq!(
        names,
        [
            "c/__id5__",
            "c/__id10__",
            "c/a",
            "c/aa",
            "c/b",
            "c/\u{ff5e}",
            "c/\u{1f600}"
        ]
    );
    let sub = collect(|visit| store.scan_collection(DB, &path("c/a/sub"), ReadTime::MAX, visit));
    assert_eq!(sub, ["c/a/sub/x"]);
}

#[test]
fn scans_are_snapshots_and_can_stop_early() {
    let (store, now) = store_with_clock(1_000);
    let t1 = set(&store, "c/a", 1);
    now.store(2_000, Ordering::SeqCst);
    set(&store, "c/b", 1);
    delete(&store, "c/a");

    assert_eq!(
        collect(|v| store.scan_collection(DB, &path("c"), t1, v)),
        ["c/a"]
    );
    assert_eq!(
        collect(|v| store.scan_collection(DB, &path("c"), ReadTime::MAX, v)),
        ["c/b"]
    );

    set(&store, "c/c", 1);
    let mut seen = 0;
    store.scan_collection(DB, &path("c"), ReadTime::MAX, &mut |_| {
        seen += 1;
        ControlFlow::Break(())
    });
    assert_eq!(seen, 1);
}

#[test]
fn collection_groups_cover_every_depth_in_full_path_order() {
    let store = MemoryStore::new();
    for p in [
        "items/top",
        "c/b/items/x",
        "c/a/items/y",
        "c/a/items/x",
        "c/a/sub/b/items/x",
        "c-b/x/items/x",
        "c/a/other/x",
    ] {
        set(&store, p, 0);
    }
    let all = collect(|v| {
        store.scan_collection_group(DB, &ResourcePath::root(), "items", ReadTime::MAX, v);
    });
    assert_eq!(
        all,
        [
            "c/a/items/x",
            "c/a/items/y",
            "c/a/sub/b/items/x",
            "c/b/items/x",
            "c-b/x/items/x",
            "items/top",
        ]
    );
    let under_c_a = collect(|v| {
        store.scan_collection_group(DB, &path("c/a"), "items", ReadTime::MAX, v);
    });
    assert_eq!(
        under_c_a,
        ["c/a/items/x", "c/a/items/y", "c/a/sub/b/items/x"]
    );
}

#[test]
fn collection_ids_include_collections_with_only_nested_documents() {
    let (store, now) = store_with_clock(1_000);
    set(&store, "users/alice", 1);
    set(&store, "users/alice/posts/p1", 1);
    set(&store, "users/alice/drafts/d1", 1);
    // No document at ghosts/g, but ghosts/g/x/y exists: `ghosts` is still a collection.
    set(&store, "ghosts/g/x/y", 1);
    let t = set(&store, "tmp/t", 1);
    now.store(2_000, Ordering::SeqCst);
    delete(&store, "tmp/t");

    assert_eq!(
        store.list_collection_ids(DB, &ResourcePath::root(), ReadTime::MAX),
        ["ghosts", "users"]
    );
    assert_eq!(
        store.list_collection_ids(DB, &ResourcePath::root(), t),
        ["ghosts", "tmp", "users"]
    );
    assert_eq!(
        store.list_collection_ids(DB, &path("users/alice"), ReadTime::MAX),
        ["drafts", "posts"]
    );
    assert!(
        store
            .list_collection_ids(DB, &path("users/bob"), ReadTime::MAX)
            .is_empty()
    );
}

#[test]
fn versions_older_than_the_retention_window_are_dropped() {
    let (store, now) = store_with_clock(100);
    let store = store.with_retention(Duration::from_micros(10));
    set(&store, "c/d", 1); // t=100
    now.store(200, Ordering::SeqCst);
    set(&store, "c/d", 2); // t=200, cutoff 190: v1 is still the version at 190
    assert_eq!(
        n_of(&store.get(DB, &path("c/d"), ReadTime(150)).unwrap()),
        1
    );
    now.store(300, Ordering::SeqCst);
    set(&store, "c/d", 3); // t=300, cutoff 290: v2 answers reads at 290, v1 is unreachable
    assert_eq!(store.get(DB, &path("c/d"), ReadTime(150)), None);
    assert_eq!(
        n_of(&store.get(DB, &path("c/d"), ReadTime(250)).unwrap()),
        2
    );
    assert_eq!(
        n_of(&store.get(DB, &path("c/d"), ReadTime(300)).unwrap()),
        3
    );
}

#[test]
fn databases_are_isolated_and_clear_drops_everything() {
    let store = MemoryStore::new();
    set(&store, "c/d", 1);
    let other = "projects/demo/databases/second";
    assert!(store.get(other, &path("c/d"), ReadTime::MAX).is_none());
    store.clear();
    assert!(store.get(DB, &path("c/d"), ReadTime::MAX).is_none());
}

#[test]
fn read_time_round_trips_through_timestamps() {
    for micros in [
        0,
        1,
        999_999,
        1_000_000,
        -1,
        -1_000_001,
        1_760_000_000_123_456,
    ] {
        assert_eq!(
            ReadTime::from_timestamp(&ReadTime(micros).to_timestamp()),
            ReadTime(micros)
        );
    }
}
