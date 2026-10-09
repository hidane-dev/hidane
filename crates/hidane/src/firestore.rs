//! `google.firestore.v1.Firestore` service. Every RPC currently answers `UNIMPLEMENTED`
//! through the generated default stubs; the real implementations land with the v0.1 issues.

use hidane_proto::google::firestore::v1::firestore_server::Firestore;

#[derive(Debug, Default)]
pub struct FirestoreService;

impl Firestore for FirestoreService {}
