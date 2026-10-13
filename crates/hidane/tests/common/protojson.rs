//! Just enough ProtoJSON → proto to replay the REST requests recorded in the fixtures over gRPC.
//! Doubles are read as the official emulator reads them over REST: a zero is `0.0`, whatever
//! its sign (`tools/oracle/negative_zero.py`).

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
use hidane_proto::ordered::Fields;
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
    // `{}`: a value without a type.
    if json.as_object().is_some_and(serde_json::Map::is_empty) {
        return Value { value_type: None };
    }
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
            other => double(other),
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
            latitude: inner.get("latitude").map_or(0.0, double),
            longitude: inner.get("longitude").map_or(0.0, double),
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

/// A JSON number as REST reads it: `-0.0` is `0.0`.
fn double(json: &Json) -> f64 {
    let d = json.as_f64().unwrap();
    if d == 0.0 { 0.0 } else { d }
}

fn array(json: &Json) -> ArrayValue {
    ArrayValue {
        values: json["values"]
            .as_array()
            .map(|vs| vs.iter().map(value).collect())
            .unwrap_or_default(),
    }
}

pub fn fields(json: &Json) -> Fields {
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
        Some(TransformType::SetToServerValue(match kind.as_str() {
            Some("REQUEST_TIME") => 1,
            Some("SERVER_VALUE_UNSPECIFIED") => 0,
            _ => panic!("unknown server value {kind}"),
        }))
    } else if let Some(v) = json.get("increment") {
        Some(TransformType::Increment(value(v)))
    } else if let Some(v) = json.get("maximum") {
        Some(TransformType::Maximum(value(v)))
    } else if let Some(v) = json.get("minimum") {
        Some(TransformType::Minimum(value(v)))
    } else if let Some(v) = json.get("appendMissingElements") {
        Some(TransformType::AppendMissingElements(array(v)))
    } else if let Some(v) = json.get("removeAllFromArray") {
        Some(TransformType::RemoveAllFromArray(array(v)))
    } else {
        // A transform without a type, which only `fieldPath` is left for.
        assert_eq!(
            json.as_object().map(serde_json::Map::len),
            Some(1),
            "{json}"
        );
        None
    };
    FieldTransform {
        field_path: json["fieldPath"].as_str().unwrap().to_owned(),
        transform_type,
    }
}

fn write(json: &Json) -> Write {
    // `{}`: a write without an operation.
    if json.as_object().is_some_and(serde_json::Map::is_empty) {
        return Write::default();
    }
    let operation = if let Some(update) = json.get("update") {
        Operation::Update(Document {
            name: update["name"].as_str().unwrap().to_owned(),
            fields: fields(&update["fields"]),
            ..Document::default()
        })
    } else if let Some(name) = json.get("delete") {
        Operation::Delete(name.as_str().unwrap().to_owned())
    } else if let Some(name) = json.get("verify") {
        Operation::Verify(name.as_str().unwrap().to_owned())
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
/// distinct, NaN equals NaN, and `-0.0` differs from `0.0`. Timestamps are skipped; tests
/// check them separately.
pub fn same(a: &Value, b: &Value) -> bool {
    match (&a.value_type, &b.value_type) {
        (Some(ValueType::DoubleValue(x)), Some(ValueType::DoubleValue(y))) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
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

/// Same names and values, whatever their order (the order is checked by field_order.rs).
pub fn same_fields(a: &Fields, b: &Fields) -> bool {
    a.len() == b.len()
        && a.sorted()
            .into_iter()
            .zip(b.sorted())
            .all(|((ka, va), (kb, vb))| ka == kb && same(va, vb))
}
