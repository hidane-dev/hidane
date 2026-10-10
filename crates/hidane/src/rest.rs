//! REST: the HTTP/JSON transcoding of `google.firestore.v1.Firestore`, as curl, firebase-tools,
//! the Emulator UI and the web SDK's Lite build use it.
//!
//! Requests are mapped to the same RPC handlers gRPC uses, following the `google.api.http`
//! bindings of `firestore.proto`, under `/v1/` and `/v1beta1/`. JSON is ProtoJSON, read with
//! prost-reflect from the embedded descriptors and written in the format of the official
//! emulator (protobuf-java's `JsonFormat`): two-space indentation, `[{` … `}, {` … `}]` for
//! repeated messages, doubles as Java prints them, and server streams as a JSON array with one
//! message per element. Errors are one line, with `/` escaped. Unknown query parameters (such as
//! `key`) are ignored, the content type is not checked, and an empty body is `{}`, all as on the
//! official emulator; a body that is not valid for the request answers 400 "Payload isn't valid
//! for request.".

use std::{fmt::Write as _, sync::OnceLock};

use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hidane_proto::google::firestore::v1::firestore_server::Firestore;
use percent_encoding::percent_decode_str;
use prost::Message;
use prost_reflect::{
    DescriptorPool, DynamicMessage, Kind, MapKey, MessageDescriptor, ReflectMessage, Value,
};
use serde_json::{Map, Value as Json};
use tokio_stream::StreamExt;
use tonic::{Code, Status, codegen::BoxStream};

use crate::FirestoreService;

const INVALID_PAYLOAD: &str = "Payload isn't valid for request.";
/// Larger than any request the SDKs send.
const MAX_BODY: usize = 64 * 1024 * 1024;

fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(hidane_proto::FILE_DESCRIPTOR_SET).expect("embedded descriptors")
    })
}

/// A message of `google.firestore.v1`, or a full name.
fn descriptor(name: &str) -> MessageDescriptor {
    let full = if name.contains('.') {
        name.to_owned()
    } else {
        format!("google.firestore.v1.{name}")
    };
    pool()
        .get_message_by_name(&full)
        .expect("message in the embedded descriptors")
}

/// A REST request, parsed down to the RPC it maps to.
struct Call {
    method: Method,
    /// `projects/{p}/databases/{d}`.
    database: String,
    /// The path below `documents`, as segments.
    path: Vec<String>,
    verb: Option<String>,
    query: Vec<(String, String)>,
    body: Json,
    authorization: Option<String>,
}

/// Handles `/v1/…` and `/v1beta1/…`. `None` when the request is not a Firestore REST call, so
/// the caller can answer 404.
pub async fn handle(
    service: FirestoreService,
    method: Method,
    path: &str,
    raw_query: Option<&str>,
    headers: &HeaderMap,
    body: Body,
) -> Option<Response> {
    let rest = path
        .strip_prefix("/v1/")
        .or_else(|| path.strip_prefix("/v1beta1/"))?;
    let mut segments: Vec<String> = rest
        .split('/')
        .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
        .collect();
    let verb = match segments.last_mut()?.split_once(':') {
        Some((last, verb)) => {
            let verb = verb.to_owned();
            let last = last.to_owned();
            *segments.last_mut()? = last;
            Some(verb)
        }
        None => None,
    };
    let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
    let [
        "projects",
        project,
        "databases",
        database,
        "documents",
        path @ ..,
    ] = parts.as_slice()
    else {
        return None;
    };
    let bytes = to_bytes(body, MAX_BODY).await.ok()?;
    let body = if bytes.iter().all(u8::is_ascii_whitespace) {
        Json::Object(Map::new())
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(json) => json,
            Err(_) => return Some(error(&Status::invalid_argument(INVALID_PAYLOAD))),
        }
    };
    let call = Call {
        method,
        database: format!("projects/{project}/databases/{database}"),
        path: path.iter().map(|s| (*s).to_owned()).collect(),
        verb,
        query: parse_query(raw_query.unwrap_or_default()),
        body,
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    dispatch(&service, call).await
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| {
                percent_decode_str(&s.replace('+', " "))
                    .decode_utf8_lossy()
                    .into_owned()
            };
            (decode(key), decode(value))
        })
        .collect()
}

async fn dispatch(service: &FirestoreService, call: Call) -> Option<Response> {
    let documents = if call.path.is_empty() {
        format!("{}/documents", call.database)
    } else {
        format!("{}/documents/{}", call.database, call.path.join("/"))
    };
    let even = !call.path.is_empty() && call.path.len().is_multiple_of(2);
    let odd = call.path.len() % 2 == 1;
    let parent_and_collection = || {
        let (collection, parent) = call.path.split_last().expect("odd length");
        let parent = if parent.is_empty() {
            format!("{}/documents", call.database)
        } else {
            format!("{}/documents/{}", call.database, parent.join("/"))
        };
        (parent, collection.clone())
    };
    let root = call.path.is_empty();
    let unary = |response: Result<Vec<u8>, Status>, name: &str| match response {
        Ok(bytes) => json_response(&print_top(&descriptor(name), &bytes)),
        Err(status) => error(&status),
    };
    Some(match (&call.method, call.verb.as_deref()) {
        (&Method::GET, None) if even => {
            let request = build(
                &call,
                "GetDocumentRequest",
                Json::Null,
                &[("name", documents)],
            );
            match request {
                Ok(req) => unary(service.get_document(req).await.map(encode), "Document"),
                Err(e) => error(&e),
            }
        }
        (&Method::GET, None) if odd => {
            let (parent, collection) = parent_and_collection();
            let request = build(
                &call,
                "ListDocumentsRequest",
                Json::Null,
                &[("parent", parent), ("collectionId", collection)],
            );
            match request {
                Ok(req) => unary(
                    service.list_documents(req).await.map(encode),
                    "ListDocumentsResponse",
                ),
                Err(e) => error(&e),
            }
        }
        (&Method::PATCH, None) if even => {
            let mut document = call.body.clone();
            if let Json::Object(map) = &mut document {
                map.insert("name".to_owned(), Json::String(documents));
            }
            match build_with(&call, "UpdateDocumentRequest", "document", document, &[]) {
                Ok(req) => unary(service.update_document(req).await.map(encode), "Document"),
                Err(e) => error(&e),
            }
        }
        (&Method::DELETE, None) if even => {
            match build(
                &call,
                "DeleteDocumentRequest",
                Json::Null,
                &[("name", documents)],
            ) {
                Ok(req) => unary(
                    service.delete_document(req).await.map(encode),
                    "google.protobuf.Empty",
                ),
                Err(e) => error(&e),
            }
        }
        (&Method::POST, None) if odd => {
            let (parent, collection) = parent_and_collection();
            match build_with(
                &call,
                "CreateDocumentRequest",
                "document",
                call.body.clone(),
                &[("parent", parent), ("collectionId", collection)],
            ) {
                Ok(req) => unary(service.create_document(req).await.map(encode), "Document"),
                Err(e) => error(&e),
            }
        }
        (&Method::POST, Some(verb)) => {
            let database = call.database.clone();
            match verb {
                "batchGet" if root => {
                    let request = build(
                        &call,
                        "BatchGetDocumentsRequest",
                        call.body.clone(),
                        &[("database", database)],
                    );
                    match request {
                        Ok(req) => {
                            stream(
                                service.batch_get_documents(req).await,
                                "BatchGetDocumentsResponse",
                            )
                            .await
                        }
                        Err(e) => error(&e),
                    }
                }
                "beginTransaction" if root => match build(
                    &call,
                    "BeginTransactionRequest",
                    call.body.clone(),
                    &[("database", database)],
                ) {
                    Ok(req) => unary(
                        service.begin_transaction(req).await.map(encode),
                        "BeginTransactionResponse",
                    ),
                    Err(e) => error(&e),
                },
                "commit" if root => match build(
                    &call,
                    "CommitRequest",
                    call.body.clone(),
                    &[("database", database)],
                ) {
                    Ok(req) => unary(service.commit(req).await.map(encode), "CommitResponse"),
                    Err(e) => error(&e),
                },
                "rollback" if root => match build(
                    &call,
                    "RollbackRequest",
                    call.body.clone(),
                    &[("database", database)],
                ) {
                    Ok(req) => unary(
                        service.rollback(req).await.map(encode),
                        "google.protobuf.Empty",
                    ),
                    Err(e) => error(&e),
                },
                "batchWrite" if root => match build(
                    &call,
                    "BatchWriteRequest",
                    call.body.clone(),
                    &[("database", database)],
                ) {
                    Ok(req) => unary(
                        service.batch_write(req).await.map(encode),
                        "BatchWriteResponse",
                    ),
                    Err(e) => error(&e),
                },
                "runQuery" if root || even => match build(
                    &call,
                    "RunQueryRequest",
                    call.body.clone(),
                    &[("parent", documents)],
                ) {
                    Ok(req) => stream(service.run_query(req).await, "RunQueryResponse").await,
                    Err(e) => error(&e),
                },
                "runAggregationQuery" if root || even => match build(
                    &call,
                    "RunAggregationQueryRequest",
                    call.body.clone(),
                    &[("parent", documents)],
                ) {
                    Ok(req) => {
                        stream_with_done(
                            service.run_aggregation_query(req).await,
                            "RunAggregationQueryResponse",
                        )
                        .await
                    }
                    Err(e) => error(&e),
                },
                "partitionQuery" if root || even => match build(
                    &call,
                    "PartitionQueryRequest",
                    call.body.clone(),
                    &[("parent", documents)],
                ) {
                    Ok(req) => unary(
                        service.partition_query(req).await.map(encode),
                        "PartitionQueryResponse",
                    ),
                    Err(e) => error(&e),
                },
                "listCollectionIds" if root || even => match build(
                    &call,
                    "ListCollectionIdsRequest",
                    call.body.clone(),
                    &[("parent", documents)],
                ) {
                    Ok(req) => unary(
                        service.list_collection_ids(req).await.map(encode),
                        "ListCollectionIdsResponse",
                    ),
                    Err(e) => error(&e),
                },
                "executePipeline" if root => match build(
                    &call,
                    "ExecutePipelineRequest",
                    call.body.clone(),
                    &[("database", database)],
                ) {
                    Ok(req) => {
                        stream(
                            service.execute_pipeline(req).await,
                            "ExecutePipelineResponse",
                        )
                        .await
                    }
                    Err(e) => error(&e),
                },
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn encode<T: Message>(response: tonic::Response<T>) -> Vec<u8> {
    response.into_inner().encode_to_vec()
}

/// The request message `name`, from `body` (the whole request, or `Null` when the request has
/// no body), the path fields `path` (which win over the body) and the query parameters.
fn build<T: Message + Default>(
    call: &Call,
    name: &str,
    body: Json,
    path: &[(&str, String)],
) -> Result<tonic::Request<T>, Status> {
    let mut json = match body {
        Json::Null => Json::Object(Map::new()),
        other => other,
    };
    finish(call, name, &mut json, path)
}

/// Like [`build`], for requests whose body is one field (`document`).
fn build_with<T: Message + Default>(
    call: &Call,
    name: &str,
    field: &str,
    body: Json,
    path: &[(&str, String)],
) -> Result<tonic::Request<T>, Status> {
    let mut json = Json::Object(Map::new());
    json[field] = body;
    finish(call, name, &mut json, path)
}

fn finish<T: Message + Default>(
    call: &Call,
    name: &str,
    json: &mut Json,
    path: &[(&str, String)],
) -> Result<tonic::Request<T>, Status> {
    let invalid = || Status::invalid_argument(INVALID_PAYLOAD);
    let desc = descriptor(name);
    let Json::Object(map) = json else {
        return Err(invalid());
    };
    for (key, value) in &call.query {
        set_query_param(&desc, map, key, value)?;
    }
    for (field, value) in path {
        map.insert((*field).to_owned(), Json::String(value.clone()));
    }
    let message = DynamicMessage::deserialize(desc, json.clone()).map_err(|_| invalid())?;
    let message: T = message.transcode_to().map_err(|_| invalid())?;
    let mut request = tonic::Request::new(message);
    if let Some(authorization) = &call.authorization
        && let Ok(value) = authorization.parse()
    {
        request.metadata_mut().insert("authorization", value);
    }
    Ok(request)
}

/// Sets `key` (`mask.fieldPaths`, `currentDocument.exists`, …) from a query parameter, as JSON
/// of the field's type. Parameters that name no field are ignored, as on the official emulator.
fn set_query_param(
    desc: &MessageDescriptor,
    map: &mut Map<String, Json>,
    key: &str,
    value: &str,
) -> Result<(), Status> {
    let invalid = || Status::invalid_argument(INVALID_PAYLOAD);
    let mut parts = key.split('.').peekable();
    let mut desc = desc.clone();
    let mut map = map;
    while let Some(part) = parts.next() {
        let Some(field) = desc
            .get_field_by_json_name(part)
            .or_else(|| desc.get_field_by_name(part))
        else {
            return Ok(());
        };
        let json_name = field.json_name().to_owned();
        if parts.peek().is_some() {
            let Kind::Message(inner) = field.kind() else {
                return Ok(());
            };
            let entry = map
                .entry(json_name)
                .or_insert_with(|| Json::Object(Map::new()));
            let Json::Object(next) = entry else {
                return Err(invalid());
            };
            map = next;
            desc = inner;
            continue;
        }
        let scalar = match field.kind() {
            Kind::Bool => match value {
                "true" => Json::Bool(true),
                "false" => Json::Bool(false),
                _ => return Err(invalid()),
            },
            Kind::Double | Kind::Float => value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map_or_else(|| Json::String(value.to_owned()), Json::Number),
            _ => Json::String(value.to_owned()),
        };
        if field.is_list() {
            match map
                .entry(json_name)
                .or_insert_with(|| Json::Array(Vec::new()))
            {
                Json::Array(values) => values.push(scalar),
                _ => return Err(invalid()),
            }
        } else {
            map.insert(json_name, scalar);
        }
    }
    Ok(())
}

async fn stream<T: Message + 'static>(
    response: Result<tonic::Response<BoxStream<T>>, Status>,
    name: &str,
) -> Response {
    match collect(response, name).await {
        Ok(elements) => json_response(&format!("[\n{}\n]", elements.join(",\n"))),
        Err(status) => error(&status),
    }
}

/// RunAggregationQuery: the official emulator marks the last answer `"done": true`, a field
/// the published protos lack, so gRPC cannot carry it.
async fn stream_with_done<T: Message + 'static>(
    response: Result<tonic::Response<BoxStream<T>>, Status>,
    name: &str,
) -> Response {
    match collect(response, name).await {
        Ok(mut elements) => {
            if let Some(last) = elements.last_mut()
                && let Some(body) = last.strip_suffix("\n}")
            {
                *last = format!("{body},\n  \"done\": true\n}}");
            }
            json_response(&format!("[\n{}\n]", elements.join(",\n")))
        }
        Err(status) => error(&status),
    }
}

async fn collect<T: Message + 'static>(
    response: Result<tonic::Response<BoxStream<T>>, Status>,
    name: &str,
) -> Result<Vec<String>, Status> {
    let mut stream = response?.into_inner();
    let desc = descriptor(name);
    let mut elements = Vec::new();
    while let Some(item) = stream.next().await {
        elements.push(print_top(&desc, &item?.encode_to_vec()));
    }
    Ok(elements)
}

fn json_response(body: &str) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        format!("{body}\n"),
    )
        .into_response()
}

/// The official emulator's error body: one line, with `/` escaped as Java's JSON writer does.
pub fn error(status: &Status) -> Response {
    let (http, name) = http_status(status.code());
    let mut message = String::new();
    for c in status.message().chars() {
        match c {
            '"' => message.push_str("\\\""),
            '\\' => message.push_str("\\\\"),
            '/' => message.push_str("\\/"),
            '\n' => message.push_str("\\n"),
            '\r' => message.push_str("\\r"),
            '\t' => message.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(message, "\\u{:04x}", u32::from(c));
            }
            c => message.push(c),
        }
    }
    let body = format!(
        "{{\"error\":{{\"code\":{},\"message\":\"{message}\",\"status\":\"{name}\"}}}}",
        http.as_u16()
    );
    (http, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// The HTTP mapping of gRPC status codes (`google.rpc.Code`).
fn http_status(code: Code) -> (StatusCode, &'static str) {
    match code {
        Code::Ok => (StatusCode::OK, "OK"),
        Code::Cancelled => (StatusCode::from_u16(499).expect("valid"), "CANCELLED"),
        Code::Unknown => (StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN"),
        Code::InvalidArgument => (StatusCode::BAD_REQUEST, "INVALID_ARGUMENT"),
        Code::DeadlineExceeded => (StatusCode::GATEWAY_TIMEOUT, "DEADLINE_EXCEEDED"),
        Code::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND"),
        Code::AlreadyExists => (StatusCode::CONFLICT, "ALREADY_EXISTS"),
        Code::PermissionDenied => (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        Code::ResourceExhausted => (StatusCode::TOO_MANY_REQUESTS, "RESOURCE_EXHAUSTED"),
        Code::FailedPrecondition => (StatusCode::BAD_REQUEST, "FAILED_PRECONDITION"),
        Code::Aborted => (StatusCode::CONFLICT, "ABORTED"),
        Code::OutOfRange => (StatusCode::BAD_REQUEST, "OUT_OF_RANGE"),
        Code::Unimplemented => (StatusCode::NOT_IMPLEMENTED, "UNIMPLEMENTED"),
        Code::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
        Code::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "UNAVAILABLE"),
        Code::DataLoss => (StatusCode::INTERNAL_SERVER_ERROR, "DATA_LOSS"),
        Code::Unauthenticated => (StatusCode::UNAUTHORIZED, "UNAUTHENTICATED"),
    }
}

// --- printing, in protobuf-java JsonFormat's layout --------------------------------------------

/// `bytes` (an encoded `desc`) as the official emulator prints it.
fn print_top(desc: &MessageDescriptor, bytes: &[u8]) -> String {
    let message = DynamicMessage::decode(desc.clone(), bytes).expect("our own encoding");
    let mut out = String::new();
    print_message(&message, 0, &mut out);
    out
}

fn print_message(message: &DynamicMessage, indent: usize, out: &mut String) {
    let desc = message.descriptor();
    match desc.full_name() {
        "google.protobuf.Timestamp" => {
            let seconds = message
                .get_field_by_name("seconds")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let nanos = message
                .get_field_by_name("nanos")
                .and_then(|v| v.as_i32())
                .unwrap_or(0);
            out.push_str(&json_string(&timestamp(seconds, nanos)));
            return;
        }
        "google.protobuf.Empty" => {}
        name if name.starts_with("google.protobuf.") && name.ends_with("Value") => {
            if let Some(value) = message.get_field_by_name("value") {
                print_value(
                    &value,
                    &message
                        .descriptor()
                        .get_field_by_name("value")
                        .expect("wrapper")
                        .kind(),
                    indent,
                    out,
                );
                return;
            }
        }
        _ => {}
    }
    let mut entries = Vec::new();
    for field in desc.fields() {
        if !message.has_field(&field) {
            continue;
        }
        let value = message.get_field(&field);
        let mut entry = format!(
            "{}{}: ",
            " ".repeat(indent + 2),
            json_string(field.json_name())
        );
        print_value(&value, &field.kind(), indent + 2, &mut entry);
        entries.push(entry);
    }
    if entries.is_empty() {
        let _ = write!(out, "{{\n{}}}", " ".repeat(indent));
    } else {
        let _ = write!(out, "{{\n{}\n{}}}", entries.join(",\n"), " ".repeat(indent));
    }
}

fn print_value(value: &Value, kind: &Kind, indent: usize, out: &mut String) {
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::I32(n) => out.push_str(&n.to_string()),
        Value::U32(n) => out.push_str(&n.to_string()),
        Value::I64(n) => out.push_str(&json_string(&n.to_string())),
        Value::U64(n) => out.push_str(&json_string(&n.to_string())),
        Value::F32(f) => out.push_str(&java_double(f64::from(*f))),
        Value::F64(f) => out.push_str(&java_double(*f)),
        Value::String(s) => out.push_str(&json_string(s)),
        Value::Bytes(b) => out.push_str(&json_string(&STANDARD.encode(b))),
        Value::EnumNumber(n) => match kind {
            Kind::Enum(e) if e.full_name() == "google.protobuf.NullValue" => out.push_str("null"),
            Kind::Enum(e) => match e.get_value(*n) {
                Some(v) => out.push_str(&json_string(v.name())),
                None => out.push_str(&n.to_string()),
            },
            _ => out.push_str(&n.to_string()),
        },
        Value::Message(m) => print_message(m, indent, out),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                print_value(item, kind, indent, out);
            }
            out.push(']');
        }
        Value::Map(entries) => {
            let value_kind = match kind {
                Kind::Message(entry) => entry.map_entry_value_field().kind(),
                other => other.clone(),
            };
            let mut keys: Vec<&MapKey> = entries.keys().collect();
            keys.sort_by_key(|k| map_key(k));
            let printed: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    let mut entry =
                        format!("{}{}: ", " ".repeat(indent + 2), json_string(&map_key(key)));
                    print_value(&entries[key], &value_kind, indent + 2, &mut entry);
                    entry
                })
                .collect();
            if printed.is_empty() {
                let _ = write!(out, "{{\n{}}}", " ".repeat(indent));
            } else {
                let _ = write!(out, "{{\n{}\n{}}}", printed.join(",\n"), " ".repeat(indent));
            }
        }
    }
}

fn map_key(key: &MapKey) -> String {
    match key {
        MapKey::Bool(b) => b.to_string(),
        MapKey::I32(n) => n.to_string(),
        MapKey::I64(n) => n.to_string(),
        MapKey::U32(n) => n.to_string(),
        MapKey::U64(n) => n.to_string(),
        MapKey::String(s) => s.clone(),
    }
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("strings serialize")
}

/// RFC 3339 in UTC with 0, 3, 6 or 9 fractional digits, as ProtoJSON prints timestamps.
fn timestamp(seconds: i64, nanos: i32) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let mut out = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    );
    if nanos != 0 {
        if nanos % 1_000_000 == 0 {
            let _ = write!(out, ".{:03}", nanos / 1_000_000);
        } else if nanos % 1000 == 0 {
            let _ = write!(out, ".{:06}", nanos / 1000);
        } else {
            let _ = write!(out, ".{nanos:09}");
        }
    }
    out.push('Z');
    out
}

/// Days since 1970-01-01 to (year, month, day), Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A double as Java's `Double.toString` prints it (what the official emulator's JSON shows):
/// plain with at least one fractional digit between 10^-3 and 10^7, otherwise
/// `d.dddE±n`; `NaN` and the infinities as strings. Negative zero prints as `0.0`, as on the
/// official emulator.
fn java_double(f: f64) -> String {
    if f.is_nan() {
        return json_string("NaN");
    }
    if f.is_infinite() {
        return json_string(if f > 0.0 { "Infinity" } else { "-Infinity" });
    }
    if f == 0.0 {
        return "0.0".to_owned();
    }
    let magnitude = f.abs();
    if (1e-3..1e7).contains(&magnitude) {
        let plain = format!("{f}");
        if plain.contains('.') {
            plain
        } else {
            format!("{plain}.0")
        }
    } else {
        let scientific = format!("{f:e}");
        let (mantissa, exponent) = scientific.split_once('e').expect("exponent");
        let mantissa = if mantissa.contains('.') {
            mantissa.to_owned()
        } else {
            format!("{mantissa}.0")
        };
        format!("{mantissa}E{exponent}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_print_like_java() {
        for (value, java) in [
            (1e7, "1.0E7"),
            (9_999_999.0, "9999999.0"),
            (1e21, "1.0E21"),
            (1e-7, "1.0E-7"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (123_456_789.0, "1.23456789E8"),
            (1.797_693_134_862_315_7e308, "1.7976931348623157E308"),
            (0.1, "0.1"),
            (-1.5e-10, "-1.5E-10"),
            (100.0, "100.0"),
            (1e16, "1.0E16"),
            (-0.0, "0.0"),
            (1.5, "1.5"),
            (-2.0, "-2.0"),
        ] {
            assert_eq!(java_double(value), java, "{value}");
        }
    }

    #[test]
    fn timestamps_use_three_six_or_nine_digits() {
        assert_eq!(timestamp(1_577_836_800, 0), "2020-01-01T00:00:00Z");
        assert_eq!(
            timestamp(1_577_836_800, 123_000_000),
            "2020-01-01T00:00:00.123Z"
        );
        assert_eq!(
            timestamp(1_577_836_800, 123_456_000),
            "2020-01-01T00:00:00.123456Z"
        );
        assert_eq!(
            timestamp(1_577_836_800, 1),
            "2020-01-01T00:00:00.000000001Z"
        );
        assert_eq!(timestamp(-1, 0), "1969-12-31T23:59:59Z");
        assert_eq!(timestamp(951_782_400, 0), "2000-02-29T00:00:00Z");
    }
}
