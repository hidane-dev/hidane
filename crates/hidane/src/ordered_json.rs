//! JSON for the maps that keep their order: `Document.fields`, `MapValue.fields` and
//! `AggregationResult.aggregate_fields` (#108).
//!
//! REST and WebChannel go through `DynamicMessage`, whose maps do not keep an order. They use
//! the ordered descriptors instead (`hidane_proto::ORDERED_FILE_DESCRIPTOR_SET`), where these
//! three maps are lists of `{key, value}` entries; this module turns the JSON objects of those
//! maps into such lists on the way in and back into objects on the way out, in order.

use prost_reflect::{FieldDescriptor, Kind, MessageDescriptor};
use serde_json::{Map, Value as Json};

const ORDERED: [&str; 3] = [
    "google.firestore.v1.Document.fields",
    "google.firestore.v1.MapValue.fields",
    "google.firestore.v1.AggregationResult.aggregate_fields",
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

/// Turns the lists of entries in `json` (a `desc` message, as `DynamicMessage` wrote it with
/// the ordered descriptors) back into objects, in order.
pub(crate) fn entries_to_objects(json: &mut Json, desc: &MessageDescriptor) {
    walk(json, desc, &entries_to_objects_in);
}

fn entries_to_objects_in(map: &mut Json, value_desc: &MessageDescriptor) {
    let Json::Array(entries) = map else {
        return;
    };
    let mut object = Map::new();
    for entry in std::mem::take(entries) {
        let Json::Object(mut entry) = entry else {
            continue;
        };
        let key = match entry.remove("key") {
            Some(Json::String(key)) => key,
            _ => String::new(),
        };
        let mut value = entry
            .remove("value")
            .unwrap_or_else(|| Json::Object(Map::new()));
        walk(&mut value, value_desc, &entries_to_objects_in);
        object.insert(key, value);
    }
    *map = Json::Object(object);
}

#[cfg(test)]
mod tests {
    use hidane_proto::google::firestore::v1::Document;
    use prost::Message as _;
    use prost_reflect::DynamicMessage;
    use serde_json::json;

    use super::*;
    use crate::rest::descriptor;

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

        let dynamic =
            DynamicMessage::decode(desc.clone(), document.encode_to_vec().as_slice()).unwrap();
        let mut back = serde_json::to_value(&dynamic).unwrap();
        entries_to_objects(&mut back, &desc);
        // serde_json keeps object order (preserve_order), so the text shows it.
        assert_eq!(back.to_string(), json.to_string());
    }
}
