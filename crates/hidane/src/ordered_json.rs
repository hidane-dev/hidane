//! JSON for the maps that keep their order: `Document.fields`, `MapValue.fields` and
//! `AggregationResult.aggregate_fields` (#108).
//!
//! REST and WebChannel go through `DynamicMessage`, whose maps do not keep an order. They use
//! the ordered descriptors instead (`hidane_proto::ORDERED_FILE_DESCRIPTOR_SET`), where these
//! three maps are lists of `{key, value}` entries; this module turns the JSON objects of those
//! maps into such lists on the way in. On the way out, `rest`'s printer writes the lists as
//! objects, in order.

use prost_reflect::{FieldDescriptor, Kind, MessageDescriptor};
use serde_json::{Map, Value as Json};

const ORDERED: [&str; 5] = [
    "google.firestore.v1.Document.fields",
    "google.firestore.v1.MapValue.fields",
    "google.firestore.v1.AggregationResult.aggregate_fields",
    "google.datastore.v1.Entity.properties",
    "google.datastore.v1.AggregationResult.aggregate_properties",
];

pub(crate) fn is_ordered(field: &FieldDescriptor) -> bool {
    ORDERED.contains(&field.full_name())
}

/// The message type of an ordered map's values (`Value`).
fn value_type(field: &FieldDescriptor) -> Option<MessageDescriptor> {
    let Kind::Message(entry) = field.kind() else {
        return None;
    };
    match entry.get_field_by_name("value")?.kind() {
        Kind::Message(value) => Some(value),
        _ => None,
    }
}

fn field(desc: &MessageDescriptor, key: &str) -> Option<FieldDescriptor> {
    desc.get_field_by_json_name(key)
        .or_else(|| desc.get_field_by_name(key))
}

/// Walks `json`, a `desc` message, and rewrites every ordered map with `rewrite`.
fn walk(
    json: &mut Json,
    desc: &MessageDescriptor,
    rewrite: &dyn Fn(&mut Json, &MessageDescriptor),
) {
    if desc.full_name().starts_with("google.protobuf.") {
        return;
    }
    let Json::Object(object) = json else {
        return;
    };
    for (key, value) in object.iter_mut() {
        let Some(field) = field(desc, key) else {
            continue;
        };
        if is_ordered(&field) {
            if let Some(value_desc) = value_type(&field) {
                rewrite(value, &value_desc);
            }
            continue;
        }
        let Kind::Message(message) = field.kind() else {
            continue;
        };
        if field.is_map() {
            let value_desc = match message.map_entry_value_field().kind() {
                Kind::Message(value) => value,
                _ => continue,
            };
            if let Json::Object(entries) = value {
                for entry in entries.values_mut() {
                    walk(entry, &value_desc, rewrite);
                }
            }
        } else if field.is_list() {
            if let Json::Array(items) = value {
                for item in items {
                    walk(item, &message, rewrite);
                }
            }
        } else {
            walk(value, &message, rewrite);
        }
    }
}

/// Turns the ordered maps in `json` (a `desc` message, as a client sent it) into lists of
/// entries, keeping their order.
pub(crate) fn objects_to_entries(json: &mut Json, desc: &MessageDescriptor) {
    walk(json, desc, &objects_to_entries_in);
}

fn objects_to_entries_in(map: &mut Json, value_desc: &MessageDescriptor) {
    let Json::Object(object) = map else {
        return;
    };
    let entries = std::mem::take(object)
        .into_iter()
        .map(|(key, mut value)| {
            walk(&mut value, value_desc, &objects_to_entries_in);
            let mut entry = Map::new();
            entry.insert("key".to_owned(), Json::String(key));
            entry.insert("value".to_owned(), value);
            Json::Object(entry)
        })
        .collect();
    *map = Json::Array(entries);
}

#[cfg(test)]
mod tests {
    use hidane_proto::google::firestore::v1::Document;
    use prost::Message as _;
    use prost_reflect::DynamicMessage;
    use serde_json::json;

    use super::*;
    use crate::rest::{Layout, descriptor, print_top};

    #[test]
    fn json_keeps_the_order_both_ways() {
        let json = json!({
            "name": "projects/p/databases/(default)/documents/c/d",
            "fields": {
                "zeta": {"integerValue": "1"},
                "alpha": {"arrayValue": {"values": [{"mapValue": {"fields": {
                    "b": {"integerValue": "2"}, "a": {"integerValue": "3"}
                }}}]}},
                "mid": {"mapValue": {"fields": {
                    "z": {"integerValue": "4"}, "a": {"integerValue": "5"}
                }}}
            }
        });
        let desc = descriptor("Document");

        let mut entries = json.clone();
        objects_to_entries(&mut entries, &desc);
        let document: Document = DynamicMessage::deserialize(desc.clone(), entries)
            .unwrap()
            .transcode_to()
            .unwrap();
        assert_eq!(
            document.fields.keys().collect::<Vec<_>>(),
            ["zeta", "alpha", "mid"]
        );

        // WebChannel's layout prints these as serde_json does, which keeps object order
        // (preserve_order).
        let back = print_top(&desc, &document.encode_to_vec(), Layout::WebChannel);
        assert_eq!(back, json.to_string());
    }
}
