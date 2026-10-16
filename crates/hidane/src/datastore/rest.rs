//! Datastore mode's REST API: `POST /v1/projects/{project_id}:{verb}`, the `google.api.http`
//! bindings of `google.datastore.v1.Datastore`. Bodies are read and answers printed as the
//! Firestore REST layer does ([`crate::rest`]): JSON read as Gson and protobuf-java read it,
//! anything they would not read answered "Payload isn't valid for request.". A body sent as
//! `application/x-protobuf`, as the Java and Python client libraries send it over HTTP, is
//! read and answered as protobuf, an error as a `google.rpc.Status`
//! (`tests/fixtures/datastore_protobuf.json`).

use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Method, header},
    response::{IntoResponse, Response},
};
use hidane_proto::google::datastore::v1::datastore_server::Datastore;
use percent_encoding::percent_decode_str;
use prost::Message;
use serde_json::{Map, Value as Json};
use tonic::Status;

use super::DatastoreService;
use crate::{gson, json_format, ordered_json, rest};

/// The largest body read, as for Firestore's REST API.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// The project and the verb of `/v1/projects/{project_id}:{verb}`.
pub(crate) fn route(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix("/v1/projects/")?;
    let (project, verb) = rest.split_once(':')?;
    if project.contains('/') || verb.contains('/') {
        return None;
    }
    let project = percent_decode_str(project).decode_utf8_lossy().into_owned();
    Some((project, verb.to_owned()))
}

fn invalid_payload() -> Status {
    Status::invalid_argument("Payload isn't valid for request.")
}

/// How a request and its answer are written.
#[derive(Clone, Copy)]
enum Format {
    Json,
    Protobuf,
}

/// The format a `Content-Type` names, as the official emulator tells them apart: protobuf for
/// exactly `application/x-protobuf`, JSON for none, `application/json…` or `text/plain…`;
/// anything else is not served (404).
fn format(headers: &HeaderMap) -> Option<Format> {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return Some(Format::Json);
    };
    match value.to_str().ok()?.trim() {
        "application/x-protobuf" => Some(Format::Protobuf),
        "" => Some(Format::Json),
        other if other.starts_with("application/json") || other.starts_with("text/plain") => {
            Some(Format::Json)
        }
        _ => None,
    }
}

/// A request's error: JSON, or a `google.rpc.Status` in protobuf.
fn failure(format: Format, status: &Status) -> Response {
    match format {
        Format::Json => rest::error(status),
        Format::Protobuf => {
            let (http, _) = rest::http_status(status.code());
            let body = hidane_proto::google::rpc::Status {
                code: status.code() as i32,
                message: status.message().to_owned(),
                details: Vec::new(),
            }
            .encode_to_vec();
            protobuf_response(http, body)
        }
    }
}

fn protobuf_response(http: axum::http::StatusCode, body: Vec<u8>) -> Response {
    (
        http,
        [(header::CONTENT_TYPE, "application/x-protobuf")],
        body,
    )
        .into_response()
}

/// A request in protobuf, with the path's project.
fn decode<T: Message + Default>(
    bytes: &[u8],
    project: &str,
    set: fn(&mut T, String),
) -> Result<T, Status> {
    let mut request = T::decode(bytes).map_err(|_| invalid_payload())?;
    set(&mut request, project.to_owned());
    Ok(request)
}

/// The request message of `desc` from `json`, with the path's project.
fn read<T: Message + Default>(name: &str, mut json: Json, project: &str) -> Result<T, Status> {
    let desc = rest::descriptor(&format!("google.datastore.v1.{name}"));
    let Json::Object(map) = &mut json else {
        return Err(invalid_payload());
    };
    map.insert("projectId".to_owned(), Json::String(project.to_owned()));
    ordered_json::objects_to_entries(&mut json, &desc);
    rest::read_json(desc, json)
        .ok_or_else(invalid_payload)?
        .transcode_to()
        .map_err(|_| invalid_payload())
}

fn answer<T: Message>(
    format: Format,
    name: &str,
    result: Result<tonic::Response<T>, Status>,
) -> Response {
    match (result, format) {
        (Ok(response), Format::Json) => {
            let desc = rest::descriptor(&format!("google.datastore.v1.{name}"));
            let bytes = response.into_inner().encode_to_vec();
            rest::json_response(&rest::print_top(&desc, &bytes, rest::Layout::Rest))
        }
        (Ok(response), Format::Protobuf) => protobuf_response(
            axum::http::StatusCode::OK,
            response.into_inner().encode_to_vec(),
        ),
        (Err(status), format) => failure(format, &status),
    }
}

pub(crate) async fn handle(
    service: &DatastoreService,
    method: &Method,
    headers: &HeaderMap,
    project: &str,
    verb: &str,
    body: Body,
) -> Response {
    if method != Method::POST {
        return crate::not_found_response();
    }
    let known = [
        "lookup",
        "beginTransaction",
        "commit",
        "rollback",
        "allocateIds",
        "reserveIds",
        "runQuery",
        "runAggregationQuery",
    ];
    if !known.contains(&verb) {
        return crate::not_found_response();
    }
    let Some(format) = format(headers) else {
        return crate::not_found_response();
    };
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        return failure(format, &invalid_payload());
    };
    if let Format::Protobuf = format {
        macro_rules! call {
            ($request:ident, $response:literal, $method:ident) => {
                match decode::<hidane_proto::google::datastore::v1::$request>(
                    &bytes,
                    project,
                    |r, p| r.project_id = p,
                ) {
                    Ok(request) => answer(
                        format,
                        $response,
                        service.$method(tonic::Request::new(request)).await,
                    ),
                    Err(status) => failure(format, &status),
                }
            };
        }
        return match verb {
            "lookup" => call!(LookupRequest, "LookupResponse", lookup),
            "beginTransaction" => call!(
                BeginTransactionRequest,
                "BeginTransactionResponse",
                begin_transaction
            ),
            "commit" => call!(CommitRequest, "CommitResponse", commit),
            "rollback" => call!(RollbackRequest, "RollbackResponse", rollback),
            "allocateIds" => call!(AllocateIdsRequest, "AllocateIdsResponse", allocate_ids),
            "reserveIds" => call!(ReserveIdsRequest, "ReserveIdsResponse", reserve_ids),
            "runQuery" => call!(RunQueryRequest, "RunQueryResponse", run_query),
            "runAggregationQuery" => call!(
                RunAggregationQueryRequest,
                "RunAggregationQueryResponse",
                run_aggregation_query
            ),
            _ => crate::not_found_response(),
        };
    }
    let json = if bytes.iter().all(u8::is_ascii_whitespace) {
        Json::Object(Map::new())
    } else {
        match gson::strict(&bytes).and_then(|text| {
            let mut json = serde_json::from_str::<Json>(&text).ok()?;
            json_format::mark_numbers(text.as_bytes(), &mut json);
            Some(json)
        }) {
            Some(json) => json,
            None => return rest::error(&invalid_payload()),
        }
    };
    macro_rules! call {
        ($request:literal, $response:literal, $method:ident) => {
            match read($request, json, project) {
                Ok(request) => answer(
                    format,
                    $response,
                    service.$method(tonic::Request::new(request)).await,
                ),
                Err(status) => rest::error(&status),
            }
        };
    }
    match verb {
        "lookup" => call!("LookupRequest", "LookupResponse", lookup),
        "beginTransaction" => call!(
            "BeginTransactionRequest",
            "BeginTransactionResponse",
            begin_transaction
        ),
        "commit" => call!("CommitRequest", "CommitResponse", commit),
        "rollback" => call!("RollbackRequest", "RollbackResponse", rollback),
        "allocateIds" => call!("AllocateIdsRequest", "AllocateIdsResponse", allocate_ids),
        "reserveIds" => call!("ReserveIdsRequest", "ReserveIdsResponse", reserve_ids),
        "runQuery" => call!("RunQueryRequest", "RunQueryResponse", run_query),
        "runAggregationQuery" => call!(
            "RunAggregationQueryRequest",
            "RunAggregationQueryResponse",
            run_aggregation_query
        ),
        _ => crate::not_found_response(),
    }
}
