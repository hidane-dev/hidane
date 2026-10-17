//! Datastore mode's REST API: `POST /v1/projects/{project_id}:{verb}`, the `google.api.http`
//! bindings of `google.datastore.v1.Datastore`. Bodies are read and answers printed as the
//! Firestore REST layer does ([`crate::rest`]): JSON read as Gson and protobuf-java read it,
//! anything they would not read answered "Payload isn't valid for request.".

use axum::{
    body::{Body, to_bytes},
    http::Method,
    response::Response,
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

fn answer<T: Message>(name: &str, result: Result<tonic::Response<T>, Status>) -> Response {
    match result {
        Ok(response) => {
            let desc = rest::descriptor(&format!("google.datastore.v1.{name}"));
            let bytes = response.into_inner().encode_to_vec();
            rest::json_response(&rest::print_top(&desc, &bytes, rest::Layout::Rest))
        }
        Err(status) => rest::error(&status),
    }
}

pub(crate) async fn handle(
    service: &DatastoreService,
    method: &Method,
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
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        return rest::error(&invalid_payload());
    };
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
