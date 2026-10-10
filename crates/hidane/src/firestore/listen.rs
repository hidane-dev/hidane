//! The Listen stream (`onSnapshot`, and every read of the web and mobile SDKs).
//!
//! The first answer to a target follows the official emulator: `ADD`, the matching documents
//! (in query order, or as `DocumentDelete` for a missing named document), `CURRENT` and a
//! `NO_CHANGE` for all targets with the read time; resume tokens carry that time in the
//! official encoding.
//!
//! After that, hidane sends what production Firestore sends: per commit, only the documents
//! that entered, changed in or left each target, then one `NO_CHANGE` with the commit time.
//! The official emulator instead answers every write to a watched collection with `RESET` and
//! the whole result again, which is what makes its writes slow down with listeners attached
//! (docs/parity-exceptions.md). The SDKs build the same snapshots from either.
//!
//! Queries without a limit, offset or cursor follow changes one document at a time, so a
//! commit costs O(changed documents); the others run again when a commit touches their
//! collections.
//!
//! A target resumed from a token (or read time) T gets, as in production, only what changed
//! since T: the target's documents at T and now are read from the store's versions and
//! compared. When T is older than the versions kept (one hour, ADR 0002), the documents
//! updated after T come with an `ExistenceFilter` holding the count, and the SDKs resynchronise
//! if theirs differs. Tokens from before this process started or the store was reset, and
//! tokens that do not parse, start the target over with `RESET`, as the official emulator
//! always does.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
};

use hidane_core::{
    order::compare_paths,
    path::ResourcePath,
    store::{Change, Commit, ReadTime, StoredDocument},
};
use hidane_proto::google::{
    firestore::v1::{
        DocumentChange, DocumentDelete, ExistenceFilter, ListenRequest, ListenResponse,
        StructuredQuery, Target, TargetChange, listen_request, listen_response, target,
        target::query_target, target_change::TargetChangeType,
    },
    rpc,
};
use prost_types::Timestamp;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{Stream, StreamExt};
use tonic::Status;

use super::{FirestoreService, changes::Event, names, query::Query, to_document};

type Responses = mpsc::Sender<Result<ListenResponse, Status>>;

/// What a target watches.
enum Watch {
    /// Named documents, in request order without duplicates.
    Documents(Vec<ResourcePath>),
    Query(Box<Query>),
}

struct WatchedTarget {
    watch: Watch,
    /// The documents the client currently has for this target, by update time.
    members: HashMap<ResourcePath, Timestamp>,
}

/// The client went away: stop quietly.
struct Gone;

struct Listener<'a> {
    service: &'a FirestoreService,
    responses: &'a Responses,
    database: Option<String>,
    feed: Option<broadcast::Receiver<Event>>,
    /// Events received while adding a target that are newer than its snapshot.
    pending: VecDeque<Event>,
    targets: BTreeMap<i32, WatchedTarget>,
    /// Every target reflects every commit up to this time.
    as_of: ReadTime,
    next_assigned_id: i32,
}

impl FirestoreService {
    /// Serves one Listen stream until the client half-closes or goes away. Requests and
    /// responses are plain messages, so any transport can carry them: gRPC here, WebChannel
    /// later (ADR 0004).
    pub(crate) async fn serve_listen(
        &self,
        requests: &mut (impl Stream<Item = Result<ListenRequest, Status>> + Unpin + Send),
        responses: &Responses,
    ) -> Result<(), Status> {
        let mut listener = Listener {
            service: self,
            responses,
            database: None,
            feed: None,
            pending: VecDeque::new(),
            targets: BTreeMap::new(),
            as_of: ReadTime(0),
            next_assigned_id: 1,
        };
        let result = listener.run(requests).await;
        match result {
            Ok(()) | Err(Ok(Gone)) => Ok(()),
            Err(Err(status)) => Err(status),
        }
    }
}

type Outcome<T> = Result<T, Result<Gone, Status>>;

impl Listener<'_> {
    async fn run(
        &mut self,
        requests: &mut (impl Stream<Item = Result<ListenRequest, Status>> + Unpin + Send),
    ) -> Outcome<()> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.apply_event(&event, true).await?;
                continue;
            }
            tokio::select! {
                request = requests.next() => match request.transpose().map_err(Err)? {
                    None => return Ok(()),
                    Some(request) => self.handle(request).await?,
                },
                event = recv(self.feed.as_mut()) => match event {
                    Ok(event) => self.apply_event(&event, true).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => self.resync().await?,
                    Err(broadcast::error::RecvError::Closed) => self.feed = None,
                },
            }
        }
    }

    async fn send(&self, response: listen_response::ResponseType) -> Outcome<()> {
        self.responses
            .send(Ok(ListenResponse {
                response_type: Some(response),
            }))
            .await
            .map_err(|_| Ok(Gone))
    }

    async fn target_change(
        &self,
        kind: TargetChangeType,
        target_ids: Vec<i32>,
        at: Option<ReadTime>,
        cause: Option<rpc::Status>,
    ) -> Outcome<()> {
        self.send(listen_response::ResponseType::TargetChange(TargetChange {
            target_change_type: kind as i32,
            target_ids,
            cause,
            resume_token: at.map(resume_token).unwrap_or_default(),
            read_time: at
                .filter(|_| kind != TargetChangeType::Reset)
                .map(ReadTime::to_timestamp),
        }))
        .await
    }

    /// The consistent point for every target: `NO_CHANGE` for all of them, with `at`.
    async fn snapshot(&self, at: ReadTime) -> Outcome<()> {
        self.target_change(TargetChangeType::NoChange, Vec::new(), Some(at), None)
            .await
    }

    async fn handle(&mut self, request: ListenRequest) -> Outcome<()> {
        match request.target_change {
            Some(listen_request::TargetChange::AddTarget(target)) => {
                self.add(&request.database, target).await
            }
            Some(listen_request::TargetChange::RemoveTarget(id)) => {
                // Removing a target that is not there is ignored, as on the official emulator.
                if self.targets.remove(&id).is_some() {
                    self.target_change(TargetChangeType::Remove, vec![id], None, None)
                        .await?;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    async fn add(&mut self, requested_database: &str, target: Target) -> Outcome<()> {
        let id = if target.target_id == 0 {
            while self.targets.contains_key(&self.next_assigned_id) {
                self.next_assigned_id += 1;
            }
            self.next_assigned_id
        } else {
            target.target_id
        };
        if self.targets.contains_key(&id) {
            // The official emulator ends the stream with UNKNOWN and no message here.
            return Err(Err(Status::invalid_argument(format!(
                "Target ID {id} is already in use."
            ))));
        }
        let watch = match self.parse(requested_database, &target) {
            Ok(watch) => watch,
            Err(status) => {
                // A bad target is refused on its own; the stream goes on.
                let cause = rpc::Status {
                    code: status.code() as i32,
                    message: status.message().to_owned(),
                    details: Vec::new(),
                };
                return self
                    .target_change(TargetChangeType::Remove, vec![id], None, Some(cause))
                    .await;
            }
        };
        if target.target_id == 0 {
            self.next_assigned_id = id + 1;
        }
        let database = self.database.clone().expect("set by parse");
        let changes = &self.service.changes;
        if self.feed.is_none() {
            self.feed = Some(changes.subscribe(&database));
        }
        // Bring the other targets up to the time the new one is read at.
        let at = changes.published(&database).max(self.as_of);
        self.catch_up(at).await?;

        self.target_change(TargetChangeType::Add, vec![id], None, None)
            .await?;
        let mut watched = WatchedTarget {
            watch,
            members: HashMap::new(),
        };
        let since = match &target.resume_type {
            None => None,
            Some(target::ResumeType::ResumeToken(token)) => Some(read_token(token)),
            Some(target::ResumeType::ReadTime(ts)) => Some(Some(ReadTime::from_timestamp(ts))),
        };
        let store = self.service.store.as_ref();
        match since {
            None => self.send_initial(id, &mut watched, &database, at).await?,
            Some(Some(since))
                if since <= at
                    && since >= self.service.changes.history_start(&database)
                    && since >= store.earliest_read_time(&database) =>
            {
                self.send_since(id, &mut watched, &database, since, at)
                    .await?;
            }
            Some(Some(since))
                if since <= at && since >= self.service.changes.history_start(&database) =>
            {
                self.send_with_count(id, &mut watched, &database, since, at)
                    .await?;
            }
            // A token that does not parse, is from the future, or predates this history.
            Some(_) => {
                self.target_change(TargetChangeType::Reset, vec![id], Some(at), None)
                    .await?;
                self.send_initial(id, &mut watched, &database, at).await?;
            }
        }
        self.targets.insert(id, watched);
        self.target_change(TargetChangeType::Current, vec![id], Some(at), None)
            .await?;
        self.as_of = at;
        self.snapshot(at).await
    }

    /// Applies, without the closing `NO_CHANGE`, every received commit up to `at`; newer
    /// ones wait in `pending`.
    async fn catch_up(&mut self, at: ReadTime) -> Outcome<()> {
        let mut caught_up = Vec::new();
        while let Some(commit) = self.pending.pop_front() {
            caught_up.push(commit);
        }
        let mut lagged = false;
        if let Some(feed) = self.feed.as_mut() {
            loop {
                match feed.try_recv() {
                    Ok(commit) => caught_up.push(commit),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => lagged = true,
                    Err(_) => break,
                }
            }
        }
        if lagged {
            // Some events are lost: start every target over at `at`.
            self.pending
                .extend(caught_up.into_iter().filter(|e| e.time() > at));
            return self.resync_at(at).await;
        }
        for event in caught_up {
            if event.time() <= at {
                self.apply_event(&event, false).await?;
            } else {
                self.pending.push_back(event);
            }
        }
        Ok(())
    }

    fn parse(&mut self, requested_database: &str, target: &Target) -> Result<Watch, Status> {
        let (database, watch) = match &target.target_type {
            Some(target::TargetType::Query(query)) => {
                let parent = names::parent(&query.parent)?;
                let default = StructuredQuery::default();
                let structured = match &query.query_type {
                    Some(query_target::QueryType::StructuredQuery(q)) => q,
                    None => &default,
                };
                let query = Query::parse(parent.path, structured)?;
                (parent.database, Watch::Query(Box::new(query)))
            }
            Some(target::TargetType::Documents(documents)) => {
                let mut database = None;
                let mut paths = Vec::new();
                for name in &documents.documents {
                    let parsed = names::document(name)?;
                    if database.get_or_insert_with(|| parsed.database.clone()) != &parsed.database {
                        return Err(Status::invalid_argument(
                            "All documents of a target must be in one database.",
                        ));
                    }
                    if !paths.contains(&parsed.path) {
                        paths.push(parsed.path);
                    }
                }
                let database = database
                    .or_else(|| {
                        (!requested_database.is_empty()).then(|| requested_database.to_owned())
                    })
                    .ok_or_else(|| Status::invalid_argument("A target needs documents."))?;
                (database, Watch::Documents(paths))
            }
            None => {
                return Err(Status::invalid_argument(
                    "A target needs a query or documents.",
                ));
            }
        };
        // One stream watches one database, the one its first target names.
        let stream_database = self.database.get_or_insert_with(|| database.clone());
        if *stream_database != database {
            return Err(Status::invalid_argument(format!(
                "Target is in database \"{database}\", but the stream watches \"{stream_database}\"."
            )));
        }
        Ok(watch)
    }

    async fn send_initial(
        &self,
        id: i32,
        watched: &mut WatchedTarget,
        database: &str,
        at: ReadTime,
    ) -> Outcome<()> {
        let store = self.service.store.as_ref();
        match &watched.watch {
            Watch::Documents(paths) => {
                for path in paths {
                    match store.get(database, path, at) {
                        Some(doc) => {
                            watched.members.insert(path.clone(), doc.update_time);
                            self.document_change(id, database, &doc, None).await?;
                        }
                        None => self.document_gone(id, database, path, at, None).await?,
                    }
                }
            }
            Watch::Query(query) => {
                for doc in query.run(store, database, at).documents {
                    watched.members.insert((*doc.path).clone(), doc.update_time);
                    self.document_change(id, database, &doc, query.projection())
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// The documents of a target at `at`, in query (or request) order.
    fn documents_at(
        &self,
        watch: &Watch,
        database: &str,
        at: ReadTime,
    ) -> Vec<Arc<StoredDocument>> {
        let store = self.service.store.as_ref();
        match watch {
            Watch::Documents(paths) => paths
                .iter()
                .filter_map(|path| store.get(database, path, at))
                .collect(),
            Watch::Query(query) => query.run(store, database, at).documents,
        }
    }

    /// Resumes a target from `since`: what entered, changed in or left it between `since` and
    /// `at`.
    async fn send_since(
        &self,
        id: i32,
        watched: &mut WatchedTarget,
        database: &str,
        since: ReadTime,
        at: ReadTime,
    ) -> Outcome<()> {
        let before: HashMap<ResourcePath, Timestamp> = self
            .documents_at(&watched.watch, database, since)
            .iter()
            .map(|doc| ((*doc.path).clone(), doc.update_time))
            .collect();
        let projection = match &watched.watch {
            Watch::Query(query) => query.projection(),
            Watch::Documents(_) => None,
        };
        let now = self.documents_at(&watched.watch, database, at);
        for doc in &now {
            watched.members.insert((*doc.path).clone(), doc.update_time);
            if before.get(&*doc.path) != Some(&doc.update_time) {
                self.document_change(id, database, doc, projection).await?;
            }
        }
        let store = self.service.store.as_ref();
        let mut gone: Vec<&ResourcePath> = before
            .keys()
            .filter(|path| !watched.members.contains_key(*path))
            .collect();
        gone.sort_by(|a, b| compare_paths(a.segments(), b.segments()));
        for path in gone {
            let still_there = store.get(database, path, at);
            self.document_gone(id, database, path, at, still_there.as_deref())
                .await?;
        }
        Ok(())
    }

    /// Resumes a target from `since`, older than the versions kept: the documents updated
    /// after it, and the count, so a client holding others can tell and start over.
    async fn send_with_count(
        &self,
        id: i32,
        watched: &mut WatchedTarget,
        database: &str,
        since: ReadTime,
        at: ReadTime,
    ) -> Outcome<()> {
        let projection = match &watched.watch {
            Watch::Query(query) => query.projection(),
            Watch::Documents(_) => None,
        };
        let now = self.documents_at(&watched.watch, database, at);
        let since = since.to_timestamp();
        for doc in &now {
            watched.members.insert((*doc.path).clone(), doc.update_time);
            let updated_after =
                (doc.update_time.seconds, doc.update_time.nanos) > (since.seconds, since.nanos);
            if updated_after {
                self.document_change(id, database, doc, projection).await?;
            }
        }
        self.send(listen_response::ResponseType::Filter(ExistenceFilter {
            target_id: id,
            count: i32::try_from(now.len()).unwrap_or(i32::MAX),
            unchanged_names: None,
        }))
        .await
    }

    async fn document_change(
        &self,
        id: i32,
        database: &str,
        doc: &StoredDocument,
        projection: Option<&[hidane_core::field_path::FieldPath]>,
    ) -> Outcome<()> {
        self.send(listen_response::ResponseType::DocumentChange(
            DocumentChange {
                document: Some(to_document(database, doc, projection)),
                target_ids: vec![id],
                removed_target_ids: Vec::new(),
            },
        ))
        .await
    }

    /// A document left target `id`. Deleted: `DocumentDelete`. Still there: a `DocumentChange`
    /// with its new state and the target in `removed_target_ids`, as production sends; with
    /// `DocumentRemove` instead, the web SDK keeps the document until it has looked it up
    /// again, which shows as an extra snapshot.
    async fn document_gone(
        &self,
        id: i32,
        database: &str,
        path: &ResourcePath,
        at: ReadTime,
        still_there: Option<&StoredDocument>,
    ) -> Outcome<()> {
        let document = super::Name::document_name(database, path);
        self.send(match still_there {
            None => listen_response::ResponseType::DocumentDelete(DocumentDelete {
                document,
                removed_target_ids: vec![id],
                read_time: Some(at.to_timestamp()),
            }),
            Some(doc) => listen_response::ResponseType::DocumentChange(DocumentChange {
                document: Some(to_document(database, doc, None)),
                target_ids: Vec::new(),
                removed_target_ids: vec![id],
            }),
        })
        .await
    }

    async fn apply_event(&mut self, event: &Event, close: bool) -> Outcome<()> {
        match event {
            Event::Commit(commit) => self.apply(commit, close).await,
            Event::Cleared(at) => self.apply_cleared(*at, close).await,
        }
    }

    /// The database was cleared: every document of every target is gone.
    async fn apply_cleared(&mut self, at: ReadTime, close: bool) -> Outcome<()> {
        if at <= self.as_of {
            return Ok(());
        }
        let Some(database) = self.database.clone() else {
            return Ok(());
        };
        let mut gone = Vec::new();
        for (id, watched) in &mut self.targets {
            let mut paths: Vec<ResourcePath> =
                watched.members.drain().map(|(path, _)| path).collect();
            paths.sort_by(|a, b| compare_paths(a.segments(), b.segments()));
            gone.extend(paths.into_iter().map(|path| (*id, path)));
        }
        for (id, path) in &gone {
            self.document_gone(*id, &database, path, at, None).await?;
        }
        self.as_of = at;
        if !gone.is_empty() && close {
            self.snapshot(at).await?;
        }
        Ok(())
    }

    /// Sends what `commit` changed in each target, then, when anything did and `close` is set,
    /// the `NO_CHANGE` that makes it a snapshot.
    async fn apply(&mut self, commit: &Commit, close: bool) -> Outcome<()> {
        if commit.commit_time <= self.as_of {
            return Ok(());
        }
        let Some(database) = self.database.clone() else {
            return Ok(());
        };
        let at = commit.commit_time;
        let changes: Vec<&Change> = commit
            .changes
            .iter()
            .filter(|c| c.before.is_some() || c.after.is_some())
            .collect();
        let mut sent = false;
        let ids: Vec<i32> = self.targets.keys().copied().collect();
        for id in ids {
            let mut watched = self.targets.remove(&id).expect("listed above");
            let result = self
                .apply_to(id, &mut watched, &database, &changes, at)
                .await;
            self.targets.insert(id, watched);
            sent |= result?;
        }
        self.as_of = at;
        if sent && close {
            self.snapshot(at).await?;
        }
        Ok(())
    }

    /// Returns whether anything was sent.
    async fn apply_to(
        &self,
        id: i32,
        watched: &mut WatchedTarget,
        database: &str,
        changes: &[&Change],
        at: ReadTime,
    ) -> Outcome<bool> {
        let mut sent = false;
        match &watched.watch {
            Watch::Documents(paths) => {
                for change in changes {
                    if !paths.contains(&change.path) {
                        continue;
                    }
                    sent = true;
                    match &change.after {
                        Some(doc) => {
                            watched
                                .members
                                .insert((*change.path).clone(), doc.update_time);
                            self.document_change(id, database, doc, None).await?;
                        }
                        None => {
                            watched.members.remove(&*change.path);
                            self.document_gone(id, database, &change.path, at, None)
                                .await?;
                        }
                    }
                }
            }
            Watch::Query(query) if query.is_incremental() => {
                for change in changes {
                    if !query.covers(&change.path) {
                        continue;
                    }
                    let matching = change
                        .after
                        .as_ref()
                        .filter(|doc| query.matches(database, doc));
                    let was_member = watched.members.contains_key(&*change.path);
                    match matching {
                        Some(doc) => {
                            sent = true;
                            watched
                                .members
                                .insert((*change.path).clone(), doc.update_time);
                            self.document_change(id, database, doc, query.projection())
                                .await?;
                        }
                        None if was_member => {
                            sent = true;
                            watched.members.remove(&*change.path);
                            self.document_gone(
                                id,
                                database,
                                &change.path,
                                at,
                                change.after.as_deref(),
                            )
                            .await?;
                        }
                        None => {}
                    }
                }
            }
            Watch::Query(query) => {
                if !changes.iter().any(|c| query.covers(&c.path)) {
                    return Ok(false);
                }
                let results = query
                    .run(self.service.store.as_ref(), database, at)
                    .documents;
                let now: HashMap<&ResourcePath, &Arc<StoredDocument>> =
                    results.iter().map(|doc| (&*doc.path, doc)).collect();
                let left: Vec<ResourcePath> = watched
                    .members
                    .keys()
                    .filter(|path| !now.contains_key(path))
                    .cloned()
                    .collect();
                for path in left {
                    sent = true;
                    watched.members.remove(&path);
                    let still_there = self.service.store.get(database, &path, at);
                    self.document_gone(id, database, &path, at, still_there.as_deref())
                        .await?;
                }
                for doc in &results {
                    if watched.members.get(&*doc.path) != Some(&doc.update_time) {
                        sent = true;
                        watched.members.insert((*doc.path).clone(), doc.update_time);
                        self.document_change(id, database, doc, query.projection())
                            .await?;
                    }
                }
            }
        }
        Ok(sent)
    }

    /// The listener fell too far behind the feed: every target starts over at the latest
    /// published time.
    async fn resync(&mut self) -> Outcome<()> {
        let Some(database) = self.database.clone() else {
            return Ok(());
        };
        let at = self.service.changes.published(&database);
        self.resync_at(at).await?;
        self.snapshot(at).await
    }

    async fn resync_at(&mut self, at: ReadTime) -> Outcome<()> {
        let Some(database) = self.database.clone() else {
            return Ok(());
        };
        self.pending.retain(|event| event.time() > at);
        let ids: Vec<i32> = self.targets.keys().copied().collect();
        for id in ids {
            let mut watched = self.targets.remove(&id).expect("listed above");
            watched.members.clear();
            let result = async {
                self.target_change(TargetChangeType::Reset, vec![id], Some(at), None)
                    .await?;
                self.send_initial(id, &mut watched, &database, at).await?;
                self.target_change(TargetChangeType::Current, vec![id], Some(at), None)
                    .await
            }
            .await;
            self.targets.insert(id, watched);
            result?;
        }
        self.as_of = at;
        Ok(())
    }
}

async fn recv(
    feed: Option<&mut broadcast::Receiver<Event>>,
) -> Result<Event, broadcast::error::RecvError> {
    match feed {
        Some(feed) => feed.recv().await,
        None => std::future::pending().await,
    }
}

/// The read time in a resume token, if it is one of ours (or the official emulator's).
fn read_token(token: &[u8]) -> Option<ReadTime> {
    let [0x0a, len, inner @ ..] = token else {
        return None;
    };
    let [0x08, varint @ ..] = inner else {
        return None;
    };
    if usize::from(*len) != inner.len() || varint.is_empty() || varint.len() > 9 {
        return None;
    }
    let mut micros = 0u64;
    for (i, byte) in varint.iter().enumerate() {
        let last = i + 1 == varint.len();
        if (byte & 0x80 == 0) != last {
            return None;
        }
        micros |= u64::from(byte & 0x7f) << (7 * i);
    }
    i64::try_from(micros).ok().map(ReadTime)
}

/// The official emulator's resume token: a message whose field 1 is a message whose field 1
/// is the read time in microseconds.
fn resume_token(at: ReadTime) -> Vec<u8> {
    let mut inner = vec![0x08];
    let mut micros = u64::try_from(at.0).unwrap_or(0);
    loop {
        let byte = u8::try_from(micros & 0x7f).expect("seven bits");
        micros >>= 7;
        if micros == 0 {
            inner.push(byte);
            break;
        }
        inner.push(byte | 0x80);
    }
    let mut token = vec![0x0a, u8::try_from(inner.len()).expect("short")];
    token.extend(inner);
    token
}
