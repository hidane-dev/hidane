//! Generated protobuf messages and gRPC services for `google.firestore.v1`, `google.datastore.v1`
//! (Datastore mode, #81; not in the descriptor sets below) and the googleapis packages they
//! depend on (`google.api`, `google.rpc`, `google.type`). Well-known types (`google.protobuf.*`)
//! come from `prost-types`.
//!
//! The protos are vendored in this crate's `proto/` directory; see `proto/README.md` there.

#[allow(clippy::all, clippy::pedantic, missing_docs, rustdoc::all)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}

pub use generated::google;

pub mod ordered;

/// Encoded `FileDescriptorSet` of every compiled proto, including imports. Served through gRPC
/// reflection so `grpcurl list` / `describe` work without local `.proto` files.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/hidane_descriptor.bin"));

/// The same descriptors, with the map fields of `Document`, `MapValue` and `AggregationResult`
/// described as repeated entries (`key`, `value`), so a `DynamicMessage` keeps their order.
pub const ORDERED_FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/hidane_descriptor_ordered.bin"));

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use crate::google::datastore::v1::{Entity, Key, key::PathElement, key::path_element::IdType};

    /// The Datastore messages are generated, and keep their wire format (#81).
    #[test]
    fn datastore_messages_round_trip() {
        let entity = Entity {
            key: Some(Key {
                partition_id: None,
                path: vec![PathElement {
                    kind: "Task".into(),
                    id_type: Some(IdType::Name("a".into())),
                }],
            }),
            properties: Default::default(),
        };
        let bytes = entity.encode_to_vec();
        assert_eq!(Entity::decode(bytes.as_slice()).unwrap(), entity);
    }

    /// Reflection lists what the descriptor set holds: Datastore is not served yet.
    #[test]
    fn the_descriptor_set_leaves_datastore_out() {
        let set = prost_types::FileDescriptorSet::decode(crate::FILE_DESCRIPTOR_SET).unwrap();
        assert!(
            set.file
                .iter()
                .all(|f| f.package() != "google.datastore.v1")
        );
        assert!(
            set.file
                .iter()
                .any(|f| f.package() == "google.firestore.v1")
        );
    }
}
