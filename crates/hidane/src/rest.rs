//! REST: the HTTP/JSON transcoding of `google.firestore.v1.Firestore`, as curl, firebase-tools,
//! the Emulator UI and the web SDK's Lite build use it.
//!
//! Requests are mapped to the same RPC handlers gRPC uses, following the `google.api.http`
//! bindings of `firestore.proto`, under `/v1/` and `/v1beta1/`. JSON is ProtoJSON, read with
//! prost-reflect from the embedded descriptors and written in the format of the official
//! emulator (protobuf-java's `JsonFormat`): two-space indentation, `[{` … `}, {` … `}]` for
//! repeated messages, doubles as Java prints them, strings escaped as [`json_string`] does, and
//! server streams as a JSON array with one message per element. Errors are one line, escaped
//! by another writer ([`error`]). Unknown query parameters (such as `key`) are ignored, the
//! content type is not checked, and an empty body is `{}`, all as on the official emulator; a
//! body is read as Gson reads it, leniently ([`gson`]), and one that is not valid for the
//! request (`null` or whitespace alone among them) answers 400 "Payload isn't valid for
//! request.".
//!
//! WebChannel prints its messages with the same printer, in a compact layout that also escapes
//! characters for HTML ([`Layout::WebChannel`]).

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

use crate::{gson, json_format, ordered_json};

use crate::FirestoreService;

const INVALID_PAYLOAD: &str = "Payload isn't valid for request.";
/// Larger than any request the SDKs send.
/// The largest body the official emulator's REST layer reads; one byte more is `413`.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// The descriptors JSON goes through: the ordered ones, where document fields and map values
/// are lists of entries, so their order survives (`ordered_json`, #108).
pub(crate) fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(hidane_proto::ORDERED_FILE_DESCRIPTOR_SET)
            .expect("embedded descriptors")
    })
}

/// A message of `google.firestore.v1`, or a full name.
pub(crate) fn descriptor(name: &str) -> MessageDescriptor {
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
    // The official emulator trims whitespace (Unicode's White_Space, as `str::trim`) around
    // every segment of the path (`tools/oracle/rest.py`).
    for segment in &mut segments {
        let trimmed = segment.trim();
        if trimmed.len() != segment.len() {
            *segment = trimmed.to_owned();
        }
    }
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
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        // The official emulator's answer to a larger body: no content.
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
        return Some(response);
    };
    // Read as Gson reads it, leniently ([`gson`]). A body it cannot read (whitespace alone among
    // them) is refused where `null` is, by the requests that read their body (`build_with`).
    let body = if bytes.is_empty() {
        Json::Object(Map::new())
    } else {
        gson::strict(&bytes)
            .and_then(|text| {
                let mut json = serde_json::from_str::<Json>(&text).ok()?;
                json_format::mark_numbers(text.as_bytes(), &mut json);
                Some(json)
            })
            .unwrap_or(Json::Null)
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

pub(crate) fn parse_query(query: &str) -> Vec<(String, String)> {
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
    // `GET …/documents/` and `GET …/documents/{document}/` list every collection under the
    // parent (an empty collection ID); any other empty segment makes a document name that
    // GetDocument refuses, as on the official emulator.
    let every_collection = call.path.split_last().is_some_and(|(last, parent)| {
        last.is_empty() && parent.len().is_multiple_of(2) && parent.iter().all(|s| !s.is_empty())
    });
    let gap = !every_collection && call.path.iter().any(String::is_empty);
    let even = !call.path.is_empty() && call.path.len().is_multiple_of(2);
    let odd = call.path.len() % 2 == 1;
    let parent_and_collection = || {
        let (collection, parent) = call.path.split_last().expect("a path below documents");
        let parent = if parent.is_empty() {
            format!("{}/documents", call.database)
        } else {
            format!("{}/documents/{}", call.database, parent.join("/"))
        };
        (parent, collection.clone())
    };
    let root = call.path.is_empty();
    let unary = |response: Result<Vec<u8>, Status>, name: &str| match response {
        Ok(bytes) => json_response(&print_top(&descriptor(name), &bytes, Layout::Rest)),
        Err(status) => error(&status),
    };
    Some(match (&call.method, call.verb.as_deref()) {
        (&Method::GET, None) if every_collection => {
            let (_, parent) = call.path.split_last().expect("a trailing segment");
            let parent = if parent.is_empty() {
                format!("{}/documents", call.database)
            } else {
                format!("{}/documents/{}", call.database, parent.join("/"))
            };
            let request = build(
                &call,
                "ListDocumentsRequest",
                Json::Null,
                &[("parent", parent), ("collectionId", String::new())],
            );
            match request {
                Ok(req) => unary(
                    service.list_documents(req).await.map(encode),
                    "ListDocumentsResponse",
                ),
                Err(e) => error(&e),
            }
        }
        (&Method::GET, None) if even || gap => {
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
        (&Method::GET, None) if odd && !gap => {
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
        // PATCH, DELETE and POST take any path below `documents`, as the official emulator's
        // bindings do (`documents/*/**`, and `documents/**/{collection_id}` for POST): a path that
        // names no document is refused by the name's checks, not with a bare 404 (#262).
        (&Method::PATCH, None) if !root => {
            let mut document = call.body.clone();
            if let Json::Object(map) = &mut document {
                map.insert("name".to_owned(), Json::String(documents));
            }
            match build_with(&call, "UpdateDocumentRequest", "document", document, &[]) {
                Ok(req) => unary(service.update_document(req).await.map(encode), "Document"),
                Err(e) => error(&e),
            }
        }
        (&Method::DELETE, None) if !root => {
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
        (&Method::POST, None) if !root => {
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
                "runQuery" => match build(
                    &call,
                    "RunQueryRequest",
                    call.body.clone(),
                    &[("parent", documents)],
                ) {
                    Ok(req) => stream(service.run_query(req).await, "RunQueryResponse").await,
                    Err(e) => error(&e),
                },
                "runAggregationQuery" => match build(
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
                "partitionQuery" => match build(
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
                "listCollectionIds" => match build(
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
    // A body of JSON `null`, or one that does not read, is not a message, for every request
    // that reads its body (`POST` and `PATCH`); `GET` and `DELETE` ignore their bodies (#262).
    if matches!(call.method, Method::POST | Method::PATCH) && call.body.is_null() {
        return Err(invalid());
    }
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
    let mut json = json.clone();
    ordered_json::objects_to_entries(&mut json, &desc);
    let message = read_json(desc, json).ok_or_else(invalid)?;
    let message: T = message.transcode_to().map_err(|_| invalid())?;
    let mut request = tonic::Request::new(message);
    if let Some(authorization) = &call.authorization
        && let Ok(value) = authorization.parse()
    {
        request.metadata_mut().insert("authorization", value);
    }
    Ok(request)
}

/// A message read from ProtoJSON as the official emulator reads it, for REST and WebChannel:
/// leniently where protobuf-java's `JsonFormat` is ([`json_format::lenient`]). It reads a
/// double through `BigDecimal`, which has no negative
/// zero, so `-0.0`, `-0` and `"-0"` (the web SDK's spelling of -0) are read as `0.0`; gRPC
/// keeps `-0.0`, and the store keeps what it is given (`tools/oracle/negative_zero.py`). A
/// literal that only rounds to zero, such as `-1e-400`, is `-0.0` on the official emulator and
/// `0.0` here (docs/parity-exceptions.md).
pub(crate) fn read_json(desc: MessageDescriptor, json: Json) -> Option<DynamicMessage> {
    fn positive_zeros(message: &mut DynamicMessage) {
        for (_, value) in message.fields_mut() {
            positive_zero(value);
        }
    }
    fn positive_zero(value: &mut Value) {
        match value {
            Value::F64(f) if *f == 0.0 => *f = 0.0,
            Value::F32(f) if *f == 0.0 => *f = 0.0,
            Value::Message(message) => positive_zeros(message),
            Value::List(values) => values.iter_mut().for_each(positive_zero),
            Value::Map(entries) => entries.values_mut().for_each(positive_zero),
            _ => {}
        }
    }
    let mut json = json;
    json_format::lenient(&mut json, &desc)?;
    let mut message = DynamicMessage::deserialize(desc, json).ok()?;
    positive_zeros(&mut message);
    Some(message)
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
        elements.push(print_top(&desc, &item?.encode_to_vec(), Layout::Rest));
    }
    Ok(elements)
}

pub(crate) fn json_response(body: &str) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        format!("{body}\n"),
    )
        .into_response()
}

/// The official emulator's error body: one line, its message escaped as [`error_escaped`] does.
pub fn error(status: &Status) -> Response {
    let (http, name) = http_status(status.code());
    let message = error_escaped(status.message());
    // An empty message is left out, as protobuf-java's JsonFormat leaves out empty strings.
    let message = if message.is_empty() {
        String::new()
    } else {
        format!("\"message\":\"{message}\",")
    };
    let body = format!(
        "{{\"error\":{{\"code\":{},{message}\"status\":\"{name}\"}}}}",
        http.as_u16()
    );
    (http, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// An error message as the official emulator's REST errors escape it, with a writer other
/// than [`json_string`]'s (`tests/fixtures/json_escapes.json`): `"`, `\` and `/` with a
/// backslash; `\b`, `\t`, `\n`, `\f` and `\r`; the other characters of U+0000–U+001F,
/// U+007F–U+009F and U+2000–U+20FF as `\u` and four uppercase hex digits. Without the quotes.
fn error_escaped(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for c in message.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2000}'..='\u{20ff}' => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// The HTTP mapping of gRPC status codes (`google.rpc.Code`).
pub(crate) fn http_status(code: Code) -> (StatusCode, &'static str) {
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

/// How a message is printed: as the official emulator prints it over REST or over WebChannel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Two-space indentation, `, ` between the elements of a list, strings escaped as
    /// [`json_string`] does.
    Rest,
    /// No whitespace, and strings (not bytes) escaped as [`html_safe_json_string`] does.
    WebChannel,
}

impl Layout {
    fn string(self, s: &str) -> String {
        match self {
            Self::Rest => json_string(s),
            Self::WebChannel => html_safe_json_string(s),
        }
    }

    /// The start of an object's entry, in an object at `indent`.
    fn key(self, key: &str, indent: usize) -> String {
        match self {
            Self::Rest => format!("{}{}: ", " ".repeat(indent + 2), self.string(key)),
            Self::WebChannel => format!("{}:", self.string(key)),
        }
    }

    fn object(self, entries: &[String], indent: usize, out: &mut String) {
        let _ = match self {
            Self::Rest if entries.is_empty() => write!(out, "{{\n{}}}", " ".repeat(indent)),
            Self::Rest => write!(out, "{{\n{}\n{}}}", entries.join(",\n"), " ".repeat(indent)),
            Self::WebChannel => write!(out, "{{{}}}", entries.join(",")),
        };
    }

    fn separator(self) -> &'static str {
        match self {
            Self::Rest => ", ",
            Self::WebChannel => ",",
        }
    }
}

/// `bytes` (an encoded `desc`) as the official emulator prints it.
pub(crate) fn print_top(desc: &MessageDescriptor, bytes: &[u8], layout: Layout) -> String {
    let message = DynamicMessage::decode(desc.clone(), bytes).expect("our own encoding");
    let mut out = String::new();
    print_message(&message, 0, layout, &mut out);
    out
}

fn print_message(message: &DynamicMessage, indent: usize, layout: Layout, out: &mut String) {
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
                    layout,
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
        let mut entry = layout.key(field.json_name(), indent);
        if ordered_json::is_ordered(&field) {
            print_ordered(&value, indent + 2, layout, &mut entry);
        } else {
            print_value(&value, &field.kind(), indent + 2, layout, &mut entry);
        }
        entries.push(entry);
    }
    layout.object(&entries, indent, out);
}

fn print_value(value: &Value, kind: &Kind, indent: usize, layout: Layout, out: &mut String) {
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::I32(n) => out.push_str(&n.to_string()),
        Value::U32(n) => out.push_str(&n.to_string()),
        Value::I64(n) => out.push_str(&json_string(&n.to_string())),
        Value::U64(n) => out.push_str(&json_string(&n.to_string())),
        Value::F32(f) => out.push_str(&java_double(f64::from(*f))),
        Value::F64(f) => out.push_str(&java_double(*f)),
        Value::String(s) => out.push_str(&layout.string(s)),
        // Base64's `=` stays as is over WebChannel too.
        Value::Bytes(b) => out.push_str(&json_string(&STANDARD.encode(b))),
        Value::EnumNumber(n) => match kind {
            Kind::Enum(e) if e.full_name() == "google.protobuf.NullValue" => out.push_str("null"),
            Kind::Enum(e) => match e.get_value(*n) {
                Some(v) => out.push_str(&json_string(v.name())),
                None => out.push_str(&n.to_string()),
            },
            _ => out.push_str(&n.to_string()),
        },
        Value::Message(m) => print_message(m, indent, layout, out),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(layout.separator());
                }
                print_value(item, kind, indent, layout, out);
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
                    let mut entry = layout.key(&map_key(key), indent);
                    print_value(&entries[key], &value_kind, indent + 2, layout, &mut entry);
                    entry
                })
                .collect();
            layout.object(&printed, indent, out);
        }
    }
}

/// An ordered map (a list of `key` / `value` entries) as the object it is, in its order.
fn print_ordered(entries: &Value, indent: usize, layout: Layout, out: &mut String) {
    let Value::List(entries) = entries else {
        return;
    };
    let printed: Vec<String> = entries
        .iter()
        .filter_map(Value::as_message)
        .map(|entry| {
            let key = entry
                .get_field_by_name("key")
                .and_then(|key| key.as_str().map(str::to_owned))
                .unwrap_or_default();
            let mut printed = layout.key(&key, indent);
            let field = entry
                .descriptor()
                .get_field_by_name("value")
                .expect("entry value");
            print_value(
                &entry.get_field(&field),
                &field.kind(),
                indent + 2,
                layout,
                &mut printed,
            );
            printed
        })
        .collect();
    layout.object(&printed, indent, out);
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

/// A JSON string as protobuf-java's JsonFormat writes it in the official emulator's REST
/// answers (`tests/fixtures/json_escapes.json`): `"` and `\` with a backslash; `\b`, `\t`,
/// `\n`, `\f` and `\r`; the other characters below U+0020, U+2028 and U+2029 as `\u` and four
/// lowercase hex digits; everything else, `/` included, as is.
pub(crate) fn json_string(s: &str) -> String {
    escaped(s, false)
}

/// A JSON string as the official emulator's WebChannel writes strings: as [`json_string`], and
/// `<`, `>`, `&`, `=` and `'` as `\u003c`, `\u003e`, `\u0026`, `\u003d` and `\u0027`.
fn html_safe_json_string(s: &str) -> String {
    escaped(s, true)
}

fn escaped(s: &str, html_safe: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\0'..='\u{1f}' | '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            '<' | '>' | '&' | '=' | '\'' if html_safe => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// RFC 3339 in UTC with 0, 3, 6 or 9 fractional digits, as ProtoJSON prints timestamps.
pub(crate) fn timestamp(seconds: i64, nanos: i32) -> String {
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
/// `d.dddE±n`; `NaN` and the infinities as strings. Negative zero prints as `-0.0`.
fn java_double(f: f64) -> String {
    if f.is_finite() {
        java_double_text(f)
    } else {
        json_string(&java_double_text(f))
    }
}

/// A double as Java's `Double.toString` prints it, `NaN` and the infinities unquoted.
pub(crate) fn java_double_text(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    let magnitude = f.abs();
    let (digits, exponent) = shortest(magnitude);
    if (1e-3..1e7).contains(&magnitude) {
        // Plain, with at least one fractional digit.
        let (whole, fraction) = match usize::try_from(exponent) {
            Ok(point) if point + 1 >= digits.len() => (
                format!("{digits}{}", "0".repeat(point + 1 - digits.len())),
                "0".to_owned(),
            ),
            Ok(point) => (digits[..=point].to_owned(), digits[point + 1..].to_owned()),
            Err(_) => (
                "0".to_owned(),
                format!(
                    "{}{digits}",
                    "0".repeat(exponent.unsigned_abs() as usize - 1)
                ),
            ),
        };
        format!("{sign}{whole}.{fraction}")
    } else if digits.len() == 1 {
        // Java prints at least two digits: when the shortest decimal has one, the two-digit
        // decimal closest to the value, which differs for a few subnormals (`4.9E-324`, where
        // the shortest is `5e-324`).
        let scientific = format!("{f:.1e}");
        let (mantissa, exponent) = scientific.split_once('e').expect("exponent");
        format!("{mantissa}E{exponent}")
    } else {
        format!("{sign}{}.{}E{exponent}", &digits[..1], &digits[1..])
    }
}

/// The digits of the shortest decimal that reads back as `magnitude` (positive and finite),
/// and the power of ten of the first. Of two equally close, Java takes the one whose last
/// digit is even (`1000000000000003.25` is `1.0000000000000032E15`), where Rust takes the
/// larger (#262).
pub(crate) fn shortest(magnitude: f64) -> (String, i32) {
    let split = |text: &str| {
        let (mantissa, exponent) = text.split_once('e').expect("exponent");
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        (digits, exponent.parse::<i32>().expect("an exponent"))
    };
    let (digits, exponent) = split(&format!("{magnitude:e}"));
    // The value exactly: a double has at most 767 significant digits.
    let (exact, exact_exponent) = split(&format!("{magnitude:.800e}"));
    if exact_exponent != exponent || exact.len() <= digits.len() {
        return (digits, exponent);
    }
    let (head, tail) = exact.split_at(digits.len());
    let tie = tail.starts_with('5') && tail[1..].bytes().all(|b| b == b'0');
    let even = |d: &str| d.as_bytes()[d.len() - 1].is_multiple_of(2);
    if !tie || even(&digits) {
        return (digits, exponent);
    }
    // The other of the two: `head` or the decimal after it, of the same length.
    let other = if digits == head {
        let mut bytes = head.as_bytes().to_vec();
        let mut i = bytes.len();
        loop {
            if i == 0 {
                return (digits, exponent);
            }
            i -= 1;
            if bytes[i] == b'9' {
                bytes[i] = b'0';
            } else {
                bytes[i] += 1;
                break;
            }
        }
        String::from_utf8(bytes).expect("digits")
    } else {
        head.to_owned()
    };
    let reads_back = format!("{}.{}e{exponent}", &other[..1], &other[1..])
        .parse::<f64>()
        .is_ok_and(|parsed| parsed == magnitude);
    if reads_back {
        (other, exponent)
    } else {
        (digits, exponent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::excessive_precision)] // the values as Java prints them, before rounding
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
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.5, "1.5"),
            (-2.0, "-2.0"),
            (1e300, "1.0E300"),
            // Recorded in tests/fixtures/function_events.json (Double.MIN_VALUE).
            (5e-324, "4.9E-324"),
            (-5e-324, "-4.9E-324"),
            (1e-323, "9.9E-324"),
            (2e-323, "2.0E-323"),
            // Two shortest decimals as close: the even one, as Java (#262).
            (1_000_000_000_000_003.25, "1.0000000000000032E15"),
            (1_000_000_000_149_213.25, "1.0000000001492132E15"),
            (123_456_789_012_345.62, "1.2345678901234562E14"),
            (1_000_000_000_035_222.75, "1.0000000000352228E15"),
            (1_000_000_000_000_003.75, "1.0000000000000038E15"),
            (4_503_599_627_370_497.5, "4.503599627370498E15"),
            (1e23, "1.0E23"),
            (9.5e21, "9.5E21"),
            (1234.5, "1234.5"),
            (0.00125, "0.00125"),
            (9_999_999.999, "9999999.999"),
            (-0.5, "-0.5"),
            (-3.25e-5, "-3.25E-5"),
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

    #[test]
    fn strings_escape_as_the_official_emulator_does() {
        let text = "a\u{1}\u{8}\u{b}\u{c}\"\\/<>&='\u{7f}\u{85}\u{a0}\u{200b}\u{2028}\u{2029}\
                    \u{20ac}\u{2100}\u{feff}\u{1f600}";
        let untouched = "\u{20ac}\u{2100}\u{feff}\u{1f600}";
        // REST bodies.
        assert_eq!(
            json_string(text),
            [
                r#""a\u0001\b\u000b\f\"\\/<>&='"#,
                "\u{7f}\u{85}\u{a0}\u{200b}",
                r"\u2028\u2029",
                untouched,
                "\""
            ]
            .concat()
        );
        // WebChannel messages.
        assert_eq!(
            html_safe_json_string(text),
            [
                r#""a\u0001\b\u000b\f\"\\/\u003c\u003e\u0026\u003d\u0027"#,
                "\u{7f}\u{85}\u{a0}\u{200b}",
                r"\u2028\u2029",
                untouched,
                "\""
            ]
            .concat()
        );
        // REST errors.
        assert_eq!(
            error_escaped(text),
            [
                r#"a\u0001\b\u000B\f\"\\\/<>&='\u007F\u0085"#,
                "\u{a0}",
                r"\u200B\u2028\u2029\u20AC",
                "\u{2100}\u{feff}\u{1f600}"
            ]
            .concat()
        );
    }

    #[test]
    fn json_has_no_negative_zero() {
        use hidane_proto::google::{firestore::v1, r#type::LatLng};

        for zero in [
            serde_json::json!(-0.0),
            serde_json::json!("-0"),
            serde_json::json!("-0.0"),
            serde_json::json!("-0e0"),
        ] {
            let json = serde_json::json!({"arrayValue": {"values": [{"doubleValue": zero}]}});
            let value: v1::Value = read_json(descriptor("Value"), json)
                .unwrap()
                .transcode_to()
                .unwrap();
            let Some(v1::value::ValueType::ArrayValue(array)) = value.value_type else {
                panic!("{value:?}")
            };
            let Some(v1::value::ValueType::DoubleValue(d)) = array.values[0].value_type else {
                panic!("{array:?}")
            };
            assert!(d == 0.0 && d.is_sign_positive(), "{zero}");

            let json = serde_json::json!({"latitude": zero, "longitude": -1.5});
            let point: LatLng = read_json(descriptor("google.type.LatLng"), json)
                .unwrap()
                .transcode_to()
                .unwrap();
            assert!(point.latitude.is_sign_positive(), "{zero}");
            assert!((point.longitude + 1.5).abs() < f64::EPSILON);
        }
    }
}
