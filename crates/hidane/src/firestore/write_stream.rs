//! The Write stream, as the web and mobile SDKs use it for writes outside transactions,
//! following the official emulator (`tests/fixtures/write_stream.json`, recorded by
//! `tools/oracle/write_stream.mjs`):
//!
//! - The first request names the database and nothing else; the answer carries a stream ID (a
//!   counter over the whole process) and the stream token `0`.
//! - Every later request is one atomic batch, answered in order with the next token (`1`, `2`,
//!   … as text), the write results and the commit time. Tokens sent by the client are not
//!   checked: the SDKs pipeline batches with the same token.
//! - A request without writes is answered with a token alone and ends the stream; so does a
//!   half-close, without an answer.
//! - Any error ends the stream with that error. Streams cannot be resumed.

use std::sync::atomic::{AtomicU64, Ordering};

use hidane_proto::google::firestore::v1::{WriteRequest, WriteResponse};
use tokio::sync::mpsc;
use tonic::{Status, Streaming};

use super::{FirestoreService, names};

/// Stream IDs count from 0 over the whole process, as on the official emulator.
static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(0);

type Responses = mpsc::Sender<Result<WriteResponse, Status>>;

impl FirestoreService {
    /// Serves one Write stream until it ends; an `Err` ends it with that status.
    pub(super) async fn serve_write_stream(
        &self,
        requests: &mut Streaming<WriteRequest>,
        responses: &Responses,
    ) -> Result<(), Status> {
        let Some(first) = requests.message().await? else {
            return Ok(());
        };
        if !first.stream_id.is_empty() {
            return Err(Status::invalid_argument(
                "Resuming streams is not supported, do not set stream ID.",
            ));
        }
        if first.database.is_empty() {
            return Err(Status::invalid_argument(
                "'database' must be set on an initial write request.",
            ));
        }
        if !first.stream_token.is_empty() {
            return Err(Status::invalid_argument(
                "Resuming streams is not supported, 'stream_token' must not be set on an initial \
                 write request.",
            ));
        }
        if !first.writes.is_empty() {
            return Err(Status::invalid_argument(
                "'writes' must not be set on an initial write request.",
            ));
        }
        // The official emulator accepts any name here (docs/parity-exceptions.md).
        let database = names::database(&first.database)?;
        let stream_id = NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed).to_string();
        let mut token = 0u64;
        if !send(
            responses,
            WriteResponse {
                stream_id,
                stream_token: token.to_string().into_bytes(),
                ..WriteResponse::default()
            },
        )
        .await
        {
            return Ok(());
        }

        while let Some(request) = requests.message().await? {
            if !request.database.is_empty() && request.database != database {
                return Err(Status::invalid_argument(format!(
                    "Request specified a database ({}) that did not match the expected database \
                     ({database})",
                    request.database
                )));
            }
            token += 1;
            let stream_token = token.to_string().into_bytes();
            if request.writes.is_empty() {
                // The SDKs send this when they close the stream.
                send(
                    responses,
                    WriteResponse {
                        stream_token,
                        ..WriteResponse::default()
                    },
                )
                .await;
                return Ok(());
            }
            let (commit_time, write_results) = self
                .write(&database, None, &request.writes)
                .await?
                .expect("a batch with writes commits");
            let response = WriteResponse {
                stream_token,
                write_results,
                commit_time: Some(commit_time.to_timestamp()),
                ..WriteResponse::default()
            };
            if !send(responses, response).await {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// `false` once the client has gone.
async fn send(responses: &Responses, response: WriteResponse) -> bool {
    responses.send(Ok(response)).await.is_ok()
}
