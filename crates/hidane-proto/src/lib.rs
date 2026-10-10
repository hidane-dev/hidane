//! Generated protobuf messages and gRPC services for `google.firestore.v1` and the googleapis
//! packages it depends on (`google.api`, `google.rpc`, `google.type`). Well-known types
//! (`google.protobuf.*`) come from `prost-types`.
//!
//! The protos are vendored in this crate's `proto/` directory; see `proto/README.md` there.

#[allow(clippy::all, clippy::pedantic, missing_docs, rustdoc::all)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}

pub use generated::google;

/// Encoded `FileDescriptorSet` of every compiled proto, including imports. Served through gRPC
/// reflection so `grpcurl list` / `describe` work without local `.proto` files.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/hidane_descriptor.bin"));
