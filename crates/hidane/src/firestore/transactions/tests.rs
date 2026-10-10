//! The lock model's timing, with tokio's paused clock: the 60 s idle timeout and the 2 s lock
//! timeout run instantly. `tests/transactions.rs` replays the official emulator's recordings
//! over gRPC in real time.

use std::sync::Arc;

use tokio::time::advance;

use super::*;

const DB: &str = "projects/p/databases/(default)";

fn path(p: &str) -> ResourcePath {
    ResourcePath::parse(p).unwrap()
}

/// A write of `doc`, in `owner` if given; returns how long it waited.
async fn write(t: &Transactions, owner: Option<&[u8]>, doc: &str) -> Result<Duration, Status> {
    let start = Instant::now();
    t.commit(DB, owner, &[path(doc)], || Ok(()))
        .await
        .map(|()| start.elapsed())
}

fn status(result: Result<impl std::fmt::Debug, Status>) -> (Code, String) {
    let err = result.unwrap_err();
    (err.code(), err.message().to_owned())
}

#[test]
fn ids_count_per_database_in_the_official_encoding() {
    let t = Transactions::default();
    assert_eq!(t.begin(DB, Mode::ReadWrite), [0x11, 1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(t.begin(DB, Mode::ReadWrite), [0x11, 2, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        t.begin("projects/p/databases/other", Mode::ReadWrite),
        encode(1)
    );
    for malformed in [&[][..], &[0, 0, 0], &[0x11, 1, 0, 0, 0, 0, 0, 0, 0, 0]] {
        assert_eq!(
            status(t.rollback(DB, malformed)),
            (Code::InvalidArgument, MALFORMED.to_owned())
        );
    }
    for never_issued in [encode(0), encode(3)] {
        assert_eq!(
            status(t.rollback(DB, &never_issued)),
            (Code::InvalidArgument, UNKNOWN.to_owned())
        );
        assert_eq!(
            status(t.read(DB, &never_issued, &[], None)),
            (Code::InvalidArgument, UNKNOWN.to_owned())
        );
    }
    t.check_retry(DB, &encode(2)).unwrap();
    assert_eq!(
        status(t.check_retry(DB, &encode(3))).0,
        Code::InvalidArgument
    );
}

#[tokio::test(start_paused = true)]
async fn a_write_gives_up_after_the_lock_timeout_and_ends_its_transaction() {
    let t = Transactions::default();
    let reader = t.begin(DB, Mode::ReadWrite);
    t.read(DB, &reader, &[path("c/d")], None).unwrap();
    let start = Instant::now();
    assert_eq!(
        status(write(&t, None, "c/d").await),
        (Code::Aborted, LOCK_TIMEOUT_MESSAGE.to_owned())
    );
    assert_eq!(start.elapsed(), LOCK_TIMEOUT);

    let writer = t.begin(DB, Mode::ReadWrite);
    assert_eq!(
        status(write(&t, Some(&writer), "c/d").await).0,
        Code::Aborted
    );
    assert_eq!(
        status(t.read(DB, &writer, &[], None)),
        (Code::Aborted, EXPIRED.to_owned())
    );
    // The reader still holds its lock and can write.
    assert_eq!(
        write(&t, Some(&reader), "c/d").await.unwrap(),
        Duration::ZERO
    );
}

#[tokio::test(start_paused = true)]
async fn a_rollback_wakes_a_waiting_write() {
    let t = Arc::new(Transactions::default());
    let id = t.begin(DB, Mode::ReadWrite);
    t.read(DB, &id, &[path("c/d")], None).unwrap();
    let waiting = tokio::spawn({
        let t = Arc::clone(&t);
        async move { write(&t, None, "c/d").await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    t.rollback(DB, &id).unwrap();
    assert_eq!(waiting.await.unwrap().unwrap(), Duration::from_millis(500));
    // Rolling back again is fine; using the transaction is not.
    t.rollback(DB, &id).unwrap();
    assert_eq!(
        status(write(&t, Some(&id), "c/d").await),
        (Code::Aborted, EXPIRED.to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn idle_transactions_expire_sixty_seconds_after_their_last_use() {
    let t = Transactions::default();
    let id = t.begin(DB, Mode::ReadWrite);
    t.read(DB, &id, &[path("c/d")], None).unwrap();
    advance(Duration::from_secs(59)).await;
    t.read(DB, &id, &[], None).unwrap(); // used again: expires at 119 s
    advance(Duration::from_secs(30)).await;
    assert_eq!(status(write(&t, None, "c/d").await).0, Code::Aborted); // 89 s to 91 s
    advance(Duration::from_secs(28)).await; // 119 s
    assert_eq!(write(&t, None, "c/d").await.unwrap(), Duration::ZERO);
    assert_eq!(
        status(write(&t, Some(&id), "c/d").await),
        (Code::Aborted, EXPIRED.to_owned())
    );
}

/// The official emulator only notices an expiry at its next request, so a write that started
/// waiting just before still times out there. hidane lets it through at the expiry.
#[tokio::test(start_paused = true)]
async fn a_waiting_write_proceeds_when_the_blocking_transaction_expires() {
    let t = Transactions::default();
    let id = t.begin(DB, Mode::ReadWrite);
    t.read(DB, &id, &[path("c/d")], None).unwrap();
    advance(Duration::from_secs(59)).await;
    assert_eq!(
        write(&t, None, "c/d").await.unwrap(),
        Duration::from_secs(1)
    );
}

#[tokio::test(start_paused = true)]
async fn collection_locks_cover_every_collection_with_that_id() {
    let t = Transactions::default();
    let id = t.begin(DB, Mode::ReadWrite);
    t.read(DB, &id, &[], Some("c")).unwrap();
    for locked in ["c/new", "x/y/c/z"] {
        assert_eq!(status(write(&t, None, locked).await).0, Code::Aborted);
    }
    for free in ["c/a/sub/x", "other/x"] {
        assert_eq!(write(&t, None, free).await.unwrap(), Duration::ZERO);
    }
    // A transaction never waits for its own locks.
    assert_eq!(write(&t, Some(&id), "c/new").await.unwrap(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn read_only_transactions_lock_nothing_and_cannot_write() {
    let t = Transactions::default();
    let id = t.begin(DB, Mode::ReadOnly(ReadTime(5)));
    assert_eq!(
        t.read(DB, &id, &[path("c/d")], Some("c")).unwrap(),
        Mode::ReadOnly(ReadTime(5))
    );
    assert_eq!(write(&t, None, "c/d").await.unwrap(), Duration::ZERO);
    assert_eq!(
        status(write(&t, Some(&id), "c/d").await),
        (Code::InvalidArgument, READ_ONLY.to_owned())
    );
    t.read(DB, &id, &[], None).unwrap(); // still open
    t.commit(DB, Some(&id), &[], || Ok(())).await.unwrap();
    assert_eq!(status(t.read(DB, &id, &[], None)).0, Code::Aborted);
}

#[tokio::test]
async fn only_an_invalid_write_leaves_the_transaction_open() {
    let t = Transactions::default();
    let id = t.begin(DB, Mode::ReadWrite);
    let invalid = t
        .commit(DB, Some(&id), &[], || {
            Err::<(), _>(Status::invalid_argument("bad"))
        })
        .await;
    assert_eq!(status(invalid).0, Code::InvalidArgument);
    t.read(DB, &id, &[], None).unwrap();
    let failed = t
        .commit(DB, Some(&id), &[], || {
            Err::<(), _>(Status::already_exists("exists"))
        })
        .await;
    assert_eq!(status(failed).0, Code::AlreadyExists);
    assert_eq!(status(t.read(DB, &id, &[], None)).0, Code::Aborted);
}

#[tokio::test(start_paused = true)]
async fn clear_forgets_transactions_but_keeps_counting() {
    let t = Transactions::default();
    let id = t.begin(DB, Mode::ReadWrite);
    t.read(DB, &id, &[path("c/d")], None).unwrap();
    t.clear();
    assert_eq!(write(&t, None, "c/d").await.unwrap(), Duration::ZERO);
    assert_eq!(
        status(write(&t, Some(&id), "c/d").await),
        (Code::Aborted, EXPIRED.to_owned())
    );
    assert_eq!(t.begin(DB, Mode::ReadWrite), encode(2));
}
