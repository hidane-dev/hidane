//! Just enough ProtoJSON → proto to replay the REST requests recorded in the fixtures over gRPC.

use std::collections::BTreeMap;

use base64::Engine as _;
use hidane_proto::google::{
    firestore::v1::{
        ArrayValue, CommitRequest, Document, DocumentMask, DocumentTransform, MapValue,
        Precondition, Value, Write,
        document_transform::{FieldTransform, field_transform::TransformType},
        precondition::ConditionType,
        value::ValueType,
        write::Operation,
    },
    r#type::LatLng,
};
use prost_types::Timestamp;
use serde_json::Value as Json;
use tonic::Code;

pub fn timestamp(text: &str) -> Timestamp {
    let t = chrono::DateTime::parse_from_rfc3339(text).unwrap();
    Timestamp {
        seconds: t.timestamp(),
        nanos: i32::try_from(t.timestamp_subsec_nanos()).unwrap(),
    }
}

pub fn value(json: &Json) -> Value {
    let (kind, inner) = json
        .as_object()
        .and_then(|o| o.iter().next())
        .expect("one field");
    let value_type = match kind.as_str() {
        "nullValue" => ValueType::NullValue(0),
        "booleanValue" => ValueType::BooleanValue(inner.as_bool().unwrap()),
        "integerValue" => ValueType::IntegerValue(match inner {
            Json::String(s) => s.parse().unwrap(),
            other => other.as_i64().unwrap(),
        }),
        "doubleValue" => ValueType::DoubleValue(match inner {
            Json::String(s) if s == "NaN" => f64::NAN,
            Json::String(s) if s == "Infinity" => f64::INFINITY,
            Json::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
            other => other.as_f64().unwrap(),
        }),
        "timestampValue" => ValueType::TimestampValue(timestamp(inner.as_str().unwrap())),
        "stringValue" => ValueType::StringValue(inner.as_str().unwrap().to_owned()),
        "bytesValue" => ValueType::BytesValue(
            base64::engine::general_purpose::STANDARD
                .decode(inner.as_str().unwrap())
                .unwrap(),
        ),
        "referenceValue" => ValueType::ReferenceValue(inner.as_str().unwrap().to_owned()),
        "geoPointValue" => ValueType::GeoPointValue(LatLng {
            latitude: inner["latitude"].as_f64().unwrap_or(0.0),
            longitude: inner["longitude"].as_f64().unwrap_or(0.0),
        }),
        "arrayValue" => ValueType::ArrayValue(array(inner)),
        "mapValue" => ValueType::MapValue(MapValue {
            fields: fields(&inner["fields"]),
        }),
        other => panic!("unsupported value kind {other}"),
    };
    Value {
        value_type: Some(value_type),
    }
}

fn array(json: &Json) -> ArrayValue {
    ArrayValue {
        values: json["values"]
            .as_array()
            .map(|vs| vs.iter().map(value).collect())
            .unwrap_or_default(),
    }
}

pub fn fields(json: &Json) -> BTreeMap<String, Value> {
    json.as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), value(v))).collect())
        .unwrap_or_default()
}

fn mask(json: &Json) -> Option<DocumentMask> {
    json.as_object().map(|_| DocumentMask {
        field_paths: json["fieldPaths"]
            .as_array()
            .map(|ps| ps.iter().map(|p| p.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default(),
    })
}

fn field_transform(json: &Json) -> FieldTransform {
    let transform_type = if let Some(kind) = json.get("setToServerValue") {
        assert_eq!(kind, "REQUEST_TIME");
        TransformType::SetToServerValue(1)
    } else if let Some(v) = json.get("increment") {
        TransformType::Increment(value(v))
    } else if let Some(v) = json.get("maximum") {
        TransformType::Maximum(value(v))
    } else if let Some(v) = json.get("minimum") {
        TransformType::Minimum(value(v))
    } else if let Some(v) = json.get("appendMissingElements") {
        TransformType::AppendMissingElements(array(v))
    } else if let Some(v) = json.get("removeAllFromArray") {
        TransformType::RemoveAllFromArray(array(v))
    } else {
        panic!("unknown transform {json}")
    };
    FieldTransform {
        field_path: json["fieldPath"].as_str().unwrap().to_owned(),
        transform_type: Some(transform_type),
    }
}

fn write(json: &Json) -> Write {
    let operation = if let Some(update) = json.get("update") {
        Operation::Update(Document {
            name: update["name"].as_str().unwrap().to_owned(),
            fields: fields(&update["fields"]),
            ..Document::default()
        })
    } else if let Some(name) = json.get("delete") {
        Operation::Delete(name.as_str().unwrap().to_owned())
    } else if let Some(t) = json.get("transform") {
        Operation::Transform(DocumentTransform {
            document: t["document"].as_str().unwrap().to_owned(),
            field_transforms: t["fieldTransforms"]
                .as_array()
                .map(|ts| ts.iter().map(field_transform).collect())
                .unwrap_or_default(),
        })
    } else {
        panic!("unknown write {json}")
    };
    let current_document = json.get("currentDocument").map(|p| Precondition {
        condition_type: Some(if let Some(exists) = p.get("exists") {
            ConditionType::Exists(exists.as_bool().unwrap())
        } else {
            ConditionType::UpdateTime(timestamp(p["updateTime"].as_str().unwrap()))
        }),
    });
    Write {
        operation: Some(operation),
        update_mask: json.get("updateMask").and_then(mask),
        update_transforms: json["updateTransforms"]
            .as_array()
            .map(|ts| ts.iter().map(field_transform).collect())
            .unwrap_or_default(),
        current_document,
    }
}

/// `POST …/documents:commit` with a REST body, as a gRPC request.
pub fn commit_request(path: &str, body: &Json) -> CommitRequest {
    CommitRequest {
        database: path.split("/documents").next().unwrap().to_owned(),
        writes: body["writes"]
            .as_array()
            .map(|ws| ws.iter().map(write).collect())
            .unwrap_or_default(),
        ..CommitRequest::default()
    }
}

/// The gRPC code for a REST error's `status` string.
pub fn code(status: &str) -> Code {
    match status {
        "INVALID_ARGUMENT" => Code::InvalidArgument,
        "NOT_FOUND" => Code::NotFound,
        "ALREADY_EXISTS" => Code::AlreadyExists,
        "PERMISSION_DENIED" => Code::PermissionDenied,
        "FAILED_PRECONDITION" => Code::FailedPrecondition,
        "ABORTED" => Code::Aborted,
        "UNIMPLEMENTED" => Code::Unimplemented,
        other => panic!("unmapped status {other}"),
    }
}

/// Structural equality for comparing with recorded REST output: integers and doubles stay
/// distinct, NaN equals NaN, and the sign of zero is ignored (the official REST output prints
/// -0.0 as 0.0). Timestamps are skipped; tests check them separately.
pub fn same(a: &Value, b: &Value) -> bool {
    match (&a.value_type, &b.value_type) {
        (Some(ValueType::DoubleValue(x)), Some(ValueType::DoubleValue(y))) => {
            (x.is_nan() && y.is_nan()) || x == y
        }
        (Some(ValueType::TimestampValue(_)), Some(ValueType::TimestampValue(_))) => true,
        (Some(ValueType::ArrayValue(x)), Some(ValueType::ArrayValue(y))) => {
            x.values.len() == y.values.len()
                && x.values.iter().zip(&y.values).all(|(p, q)| same(p, q))
        }
        (Some(ValueType::MapValue(x)), Some(ValueType::MapValue(y))) => {
            same_fields(&x.fields, &y.fields)
        }
        (x, y) => x == y,
    }
}

pub fn same_fields(a: &BTreeMap<String, Value>, b: &BTreeMap<String, Value>) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((ka, va), (kb, vb))| ka == kb && same(va, vb))
}
