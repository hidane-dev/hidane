//! The Firestore data model as hidane implements it.
//!
//! - [`order`]: how Firestore orders values (`orderBy`, cursors, range filters) and document
//!   names (`__name__`).
//! - [`key`]: an order-preserving byte encoding of the same order, for storage keys and indexes:
//!   `compare(a, b) == encode(a).cmp(&encode(b))` for every pair of values.
//! - [`path`]: document and collection paths; [`field_path`]: field paths and mask operations.
//! - [`store`]: the storage boundary and the in-memory engine (versioned documents, snapshot
//!   reads, commits that report what changed).
//! - [`transform`]: field transforms (server timestamps, increments, array unions, …).
//! - [`normalize`]: what Firestore does to a value when it is written (timestamps lose their
//!   sub-microsecond digits).
//! - [`export`]: the official emulator's export format (LevelDB logs of App Engine entities).
//!
//! Every rule here was checked against the official emulator v1.22.0; the observations live in
//! `tests/fixtures/value_order.json` and are regenerated with `tools/oracle/value_order.py`.

pub mod export;
pub mod field_path;
pub mod key;
pub mod normalize;
pub mod order;
pub mod path;
pub mod store;
pub mod transform;
