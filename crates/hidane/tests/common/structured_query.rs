//! REST `structuredQuery` JSON → `StructuredQuery`, to replay recorded queries over gRPC.

use hidane_proto::google::firestore::v1::{
    Cursor, StructuredQuery,
    structured_query::{
        CollectionSelector, CompositeFilter, Direction, FieldFilter, FieldReference, Filter,
        FindNearest, Order, Projection, UnaryFilter, composite_filter, field_filter,
        filter::FilterType, unary_filter,
    },
};
use serde_json::Value as Json;

use super::protojson::value;

pub fn field(json: &Json) -> FieldReference {
    FieldReference {
        field_path: json["fieldPath"].as_str().unwrap_or_default().to_owned(),
    }
}

fn filter(json: &Json) -> Filter {
    let filter_type = if let Some(c) = json.get("compositeFilter") {
        FilterType::CompositeFilter(CompositeFilter {
            op: c["op"]
                .as_str()
                .and_then(composite_filter::Operator::from_str_name)
                .map_or(0, |op| op as i32),
            filters: c["filters"]
                .as_array()
                .map(|fs| fs.iter().map(filter).collect())
                .unwrap_or_default(),
        })
    } else if let Some(f) = json.get("fieldFilter") {
        FilterType::FieldFilter(FieldFilter {
            field: Some(field(&f["field"])),
            op: f["op"]
                .as_str()
                .and_then(field_filter::Operator::from_str_name)
                .map_or(0, |op| op as i32),
            value: f.get("value").map(value),
        })
    } else {
        let u = &json["unaryFilter"];
        FilterType::UnaryFilter(UnaryFilter {
            op: u["op"]
                .as_str()
                .and_then(unary_filter::Operator::from_str_name)
                .map_or(0, |op| op as i32),
            operand_type: Some(unary_filter::OperandType::Field(field(&u["field"]))),
        })
    };
    Filter {
        filter_type: Some(filter_type),
    }
}

fn cursor(json: &Json) -> Cursor {
    Cursor {
        values: json["values"]
            .as_array()
            .map(|vs| vs.iter().map(value).collect())
            .unwrap_or_default(),
        before: json["before"].as_bool().unwrap_or(false),
    }
}

/// A REST `structuredQuery` as the proto message.
pub fn structured_query(json: &Json) -> StructuredQuery {
    StructuredQuery {
        select: json.get("select").map(|s| Projection {
            fields: s["fields"]
                .as_array()
                .map(|fs| fs.iter().map(field).collect())
                .unwrap_or_default(),
        }),
        from: json["from"]
            .as_array()
            .map(|cs| {
                cs.iter()
                    .map(|c| CollectionSelector {
                        collection_id: c["collectionId"].as_str().unwrap().to_owned(),
                        all_descendants: c["allDescendants"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        r#where: json.get("where").map(filter),
        order_by: json["orderBy"]
            .as_array()
            .map(|os| {
                os.iter()
                    .map(|o| Order {
                        field: Some(field(&o["field"])),
                        direction: o["direction"]
                            .as_str()
                            .and_then(Direction::from_str_name)
                            .map_or(0, |d| d as i32),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        start_at: json.get("startAt").map(cursor),
        end_at: json.get("endAt").map(cursor),
        offset: json["offset"]
            .as_i64()
            .map_or(0, |n| i32::try_from(n).unwrap()),
        limit: json["limit"].as_i64().map(|n| i32::try_from(n).unwrap()),
        find_nearest: json.get("findNearest").map(|_| FindNearest::default()),
    }
}
