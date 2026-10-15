//! `--single_project_mode` and `--single_project_mode_error`: what a request for another project
//! than `--project_id` gets, as on the official emulator (`tests/fixtures/single_project_mode.json`,
//! from `tools/oracle/single_project_mode.py`).
//!
//! - The project is the one a request is addressed to: a gRPC call's
//!   `google-cloud-resource-prefix` header, else the database of its `x-goog-request-params`
//!   header, else its name (a stream without either header is not checked); a REST path's; a
//!   WebChannel URL's `database` parameter; an export's or import's body. The names inside a
//!   request (its writes' documents, references) do not count.
//! - It is checked once the request's name is read (an invalid name is answered first), then
//!   again once the `Authorization` header is read, before anything else: each check prints a
//!   line on standard output. With `--single_project_mode` the line is a warning and the request
//!   goes on; with `--single_project_mode_error` too it is the message of the `INVALID_ARGUMENT`
//!   the request gets, so it is printed once.
//! - A WebChannel handshake refused that way is never answered.

use std::sync::{Arc, RwLock};

use tonic::{Status, metadata::MetadataMap};

/// Where the lines go: one line of text each, without its line break.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// The headers gRPC clients name their database with: the database itself, and routing
/// parameters (`database=projects%2Fp%2Fdatabases%2F(default)`).
const RESOURCE_PREFIX: &str = "google-cloud-resource-prefix";
const REQUEST_PARAMS: &str = "x-goog-request-params";

pub struct SingleProject {
    project: String,
    error: bool,
    log: RwLock<Log>,
}

impl SingleProject {
    /// `--project_id project --single_project_mode`, with `--single_project_mode_error` if
    /// `error`.
    pub fn new(project: impl Into<String>, error: bool) -> Self {
        Self {
            project: project.into(),
            error,
            log: RwLock::new(Arc::new(stdout)),
        }
    }

    /// Sends the lines to `log` instead of standard output.
    pub fn set_log(&self, log: Log) {
        *self.log.write().unwrap_or_else(|e| e.into_inner()) = log;
    }

    /// Checks `project`, reads the `Authorization` header with `read`, then checks `project`
    /// again.
    pub(crate) fn admit<T>(
        &self,
        project: &str,
        read: impl FnOnce() -> Result<T, Status>,
    ) -> Result<T, Status> {
        self.check(project)?;
        let value = read()?;
        self.check(project)?;
        Ok(value)
    }

    pub(crate) fn check(&self, project: &str) -> Result<(), Status> {
        if project == self.project {
            return Ok(());
        }
        let verdict = if self.error {
            "permitted"
        } else {
            "recommended"
        };
        let message = format!(
            "Multiple projectIds are not {verdict} in single project mode. Requested project ID \
             {project}, but the emulator is configured for {}. To opt-out of single project mode \
             add/set the '\"singleProjectMode\": false' property in the firebase.json emulators \
             config.",
            self.project
        );
        (self.log.read().unwrap_or_else(|e| e.into_inner()))(&message);
        if self.error {
            Err(Status::invalid_argument(message))
        } else {
            Ok(())
        }
    }
}

/// The project of `database` (`projects/{p}/databases/{d}`, or a name below it).
pub(crate) fn project(database: &str) -> &str {
    let rest = database.strip_prefix("projects/").unwrap_or(database);
    rest.split('/').next().unwrap_or(rest)
}

/// The project a gRPC call is addressed to: its `google-cloud-resource-prefix` header's, else
/// its `x-goog-request-params` header's, else `database`'s, if one names it.
pub(crate) fn requested(metadata: &MetadataMap, database: Option<&str>) -> Option<String> {
    let header = |name| metadata.get(name).and_then(|value| value.to_str().ok());
    let named = |database: &str| {
        database
            .starts_with("projects/")
            .then(|| project(database).to_owned())
    };
    header(RESOURCE_PREFIX)
        .and_then(named)
        .or_else(|| {
            crate::rest::parse_query(header(REQUEST_PARAMS)?)
                .into_iter()
                .find(|(key, _)| key == "database")
                .and_then(|(_, database)| named(&database))
        })
        .or_else(|| database.map(|database| project(database).to_owned()))
}

/// The official emulator prints these lines on its standard output.
fn stdout(line: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn recorded(single: &SingleProject) -> Arc<Mutex<Vec<String>>> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        single.set_log(Arc::new(move |line| {
            sink.lock().unwrap().push(line.to_owned())
        }));
        lines
    }

    #[test]
    fn another_project_is_a_warning_twice_or_an_error_once() {
        let warning = SingleProject::new("p", false);
        let lines = recorded(&warning);
        assert_eq!(warning.admit("p", || Ok(1)).unwrap(), 1);
        assert!(lines.lock().unwrap().is_empty());
        warning.admit("q", || Ok(())).unwrap();
        assert_eq!(lines.lock().unwrap().len(), 2);
        assert!(lines.lock().unwrap()[0].starts_with(
            "Multiple projectIds are not recommended in single project mode. Requested project \
             ID q, but the emulator is configured for p."
        ));

        let error = SingleProject::new("p", true);
        let lines = recorded(&error);
        let status = error.admit("q", || Ok(())).unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert_eq!(*lines.lock().unwrap(), vec![status.message().to_owned()]);
    }

    #[test]
    fn the_header_names_the_project_before_the_name() {
        let mut metadata = MetadataMap::new();
        let name = Some("projects/a/databases/d");
        assert_eq!(requested(&metadata, name).as_deref(), Some("a"));
        assert_eq!(requested(&metadata, None), None);
        let params = "database=projects%2Fc%2Fdatabases%2F(default)";
        metadata.insert(REQUEST_PARAMS, params.parse().unwrap());
        assert_eq!(requested(&metadata, name).as_deref(), Some("c"));
        metadata.insert(
            RESOURCE_PREFIX,
            "projects/b/databases/(default)".parse().unwrap(),
        );
        assert_eq!(requested(&metadata, name).as_deref(), Some("b"));
        assert_eq!(project("projects/c/databases/d/documents/x/y"), "c");
    }
}
