//! REST `structuredQuery` JSON → `StructuredQuery`, to replay recorded queries over gRPC.

use hidane_proto::google::firestore::v1::{
    Cursor, StructuredAggregationQuery, StructuredQuery,
    structured_aggregation_query::{
        self, Aggregation,
        aggregation::{Avg, Count, Operator, Sum},
    },
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
    // `{}`: a filter of no kind.
    if json.as_object().is_some_and(serde_json::Map::is_empty) {
        return Filter { filter_type: None };
    }
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

/// A REST aggregation (`count`, `sum`, `avg`, with its `alias`) as the proto message.
pub fn aggregation(json: &Json) -> Aggregation {
    let operator = if let Some(count) = json.get("count") {
        Some(Operator::Count(Count {
            up_to: count["upTo"].as_str().map(|n| n.parse().unwrap()),
        }))
    } else if let Some(sum) = json.get("sum") {
        Some(Operator::Sum(Sum {
            field: sum.get("field").map(field),
        }))
    } else {
        json.get("avg").map(|avg| {
            Operator::Avg(Avg {
                field: avg.get("field").map(field),
            })
        })
    };
    Aggregation {
        operator,
        alias: json["alias"].as_str().unwrap_or_default().to_owned(),
    }
}

/// A REST `structuredAggregationQuery` as the proto message.
pub fn structured_aggregation_query(json: &Json) -> StructuredAggregationQuery {
    StructuredAggregationQuery {
        aggregations: json["aggregations"]
            .as_array()
            .map(|a| a.iter().map(aggregation).collect())
            .unwrap_or_default(),
        query_type: json
            .get("structuredQuery")
            .map(|q| structured_aggregation_query::QueryType::StructuredQuery(structured_query(q))),
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
