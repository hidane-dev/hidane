//! The Firestore data model as hidane implements it.
//!
//! - [`order`]: how Firestore orders values (`orderBy`, cursors, range filters) and document
//!   names (`__name__`).
//! - [`key`]: an order-preserving byte encoding of the same order, for storage keys and indexes:
//!   `compare(a, b) == encode(a).cmp(&encode(b))` for every pair of values.
//! - [`path`]: document and collection paths.
//! - [`store`]: the storage boundary and the in-memory engine (versioned documents, snapshot
//!   reads, commits that report what changed).
//! - [`normalize`]: what Firestore does to a value when it is written (timestamps lose their
//!   sub-microsecond digits).
//!
//! Every rule here was checked against the official emulator v1.22.0; the observations live in
//! `tests/fixtures/value_order.json` and are regenerated with `tools/oracle/value_order.py`.

pub mod key;
pub mod normalize;
pub mod order;
pub mod path;
pub mod store;
