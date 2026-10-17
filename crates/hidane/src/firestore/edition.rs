//! The edition of each database (#36, #338): `--database-edition` is every database's, and
//! `POST /emulator/v1/projects/{p}/databases/{d}:setDatabaseEdition` sets one database's, of
//! that project alone, until `POST /reset` (`tests/fixtures/database_edition.json`, from
//! `tools/oracle/database_edition.py`). Clearing a database keeps its edition.
//!
//! The body is `{"edition": "STANDARD" | "ENTERPRISE"}`, read as protobuf-java's JSON parser
//! reads it, leniently ([`crate::gson`]): the edition's name or number, or a text or a
//! one-element array that holds either; `database` may name the path's database again. No
//! credential is needed. An edition that is neither (none, `DATABASE_EDITION_UNSPECIFIED`, an
//! unknown number) is a `500` without a message; any other key or shape is "Payload isn't valid
//! for request.", before the path's database is checked.

use std::{
    collections::HashMap,
    sync::{PoisonError, RwLock},
};

use axum::{body::Body, http::Method, response::Response};
use serde_json::Value as Json;
use tonic::Status;

use super::names;
use crate::rest;

/// Each database's edition: whether it is an enterprise one.
#[derive(Debug, Default)]
pub struct Editions {
    /// `--database-edition enterprise`: every database's, unless set.
    enterprise: bool,
    /// The databases `:setDatabaseEdition` set, by name.
    set: RwLock<HashMap<String, bool>>,
}

impl Editions {
    pub fn new(enterprise: bool) -> Self {
        Self {
            enterprise,
            set: RwLock::default(),
        }
    }

    /// Whether `database` (`projects/{p}/databases/{d}`) is an enterprise database.
    pub fn enterprise(&self, database: &str) -> bool {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(database)
            .copied()
            .unwrap_or(self.enterprise)
    }

    fn set(&self, database: String, enterprise: bool) {
        self.set
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(database, enterprise);
    }

    /// `POST /reset`: every database has `--database-edition` again.
    pub fn reset(&self) {
        self.set
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// The database `/emulator/v1/projects/{p}/databases/{d}:setDatabaseEdition` names, either ID
/// possibly empty (refused when the name is read), neither holding a `/`.
pub(crate) fn route(path: &str) -> Option<String> {
    let rest = path
        .strip_prefix("/emulator/v1/projects/")?
        .strip_suffix(":setDatabaseEdition")?;
    let (project, database) = rest.split_once("/databases/")?;
    (!project.contains('/') && !database.contains('/'))
        .then(|| format!("projects/{project}/databases/{database}"))
}

/// The largest body read (as REST bodies).
const MAX_BODY: usize = 16 * 1024 * 1024;

/// `:setDatabaseEdition` on `database`: the body, then the path's database, then the edition.
pub(crate) async fn handle(
    editions: &Editions,
    database: String,
    method: &Method,
    body: Body,
) -> Response {
    if method != Method::POST {
        return crate::not_found_response();
    }
    let result = async {
        let body = axum::body::to_bytes(body, MAX_BODY)
            .await
            .map_err(|_| invalid_payload())?;
        let request = Request::parse(&body)?;
        names::database(&database)?;
        // The official emulator never answers a body naming another database (D095).
        if !request.database.is_empty() && request.database != database {
            return Err(Status::invalid_argument(format!(
                "The body names database \"{}\", the path \"{database}\".",
                request.database
            )));
        }
        let enterprise = match request.edition {
            STANDARD => false,
            ENTERPRISE => true,
            _ => return Err(Status::unknown("")),
        };
        editions.set(database, enterprise);
        Ok(())
    }
    .await;
    match result {
        Ok(()) => crate::empty_json(),
        Err(status) => rest::error(&status),
    }
}

/// `DatabaseEdition`'s numbers.
const STANDARD: i64 = 1;
const ENTERPRISE: i64 = 2;

fn invalid_payload() -> Status {
    Status::invalid_argument("Payload isn't valid for request.")
}

#[derive(Debug, Default)]
struct Request {
    database: String,
    /// The edition's number: `DATABASE_EDITION_UNSPECIFIED` (0) when absent.
    edition: i64,
}

impl Request {
    fn parse(body: &[u8]) -> Result<Self, Status> {
        let json = if body.iter().all(u8::is_ascii_whitespace) {
            Json::Null
        } else {
            crate::gson::strict(body)
                .and_then(|text| serde_json::from_str(&text).ok())
                .ok_or_else(invalid_payload)?
        };
        let fields = match json {
            Json::Null => serde_json::Map::new(),
            Json::Object(fields) => fields,
            _ => return Err(invalid_payload()),
        };
        let mut request = Self::default();
        for (key, value) in fields {
            match key.as_str() {
                "database" => request.database = crate::export::text(value)?,
                "edition" => request.edition = edition(value)?,
                _ => return Err(invalid_payload()),
            }
        }
        Ok(request)
    }
}

/// An enum as protobuf-java reads one from JSON: `null` its default, otherwise the value's text
/// (a one-element array's element's), a name or a number.
fn edition(value: Json) -> Result<i64, Status> {
    let text = match value {
        Json::Null => return Ok(0),
        Json::Array(mut values) if values.len() == 1 => {
            return match values.pop() {
                Some(Json::Null) | None => Err(invalid_payload()),
                Some(value) => edition(value),
            };
        }
        Json::String(text) => text,
        Json::Number(number) => number.to_string(),
        Json::Bool(boolean) => boolean.to_string(),
        Json::Array(_) | Json::Object(_) => return Err(invalid_payload()),
    };
    match text.as_str() {
        "DATABASE_EDITION_UNSPECIFIED" => Ok(0),
        "STANDARD" => Ok(STANDARD),
        "ENTERPRISE" => Ok(ENTERPRISE),
        number => number
            .parse::<i32>()
            .map(i64::from)
            .map_err(|_| invalid_payload()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_name_a_database() {
        assert_eq!(
            route("/emulator/v1/projects/p/databases/(default):setDatabaseEdition").as_deref(),
            Some("projects/p/databases/(default)")
        );
        assert_eq!(
            route("/emulator/v1/projects//databases/:setDatabaseEdition").as_deref(),
            Some("projects//databases/")
        );
        assert_eq!(route("/emulator/v1/projects/p:setDatabaseEdition"), None);
        assert_eq!(
            route("/emulator/v1/projects/p/databases/a/b:setDatabaseEdition"),
            None
        );
        assert_eq!(
            route("/emulator/v1/projects/p/databases/d:setDatabaseEdition/"),
            None
        );
    }

    #[test]
    fn editions_are_read_as_protobuf_java_reads_an_enum() {
        let read = |json: Json| edition(json).map_err(|status| status.message().to_owned());
        assert_eq!(read(Json::from("ENTERPRISE")), Ok(2));
        assert_eq!(read(Json::from("2")), Ok(2));
        assert_eq!(read(Json::from(1)), Ok(1));
        assert_eq!(read(serde_json::json!(["STANDARD"])), Ok(1));
        assert_eq!(read(Json::Null), Ok(0));
        assert_eq!(read(Json::from(3)), Ok(3));
        assert!(read(Json::from("enterprise")).is_err());
        assert!(read(Json::from(true)).is_err());
        assert!(read(serde_json::json!(["STANDARD", "ENTERPRISE"])).is_err());
    }

    #[test]
    fn a_database_keeps_its_edition_until_reset() {
        let editions = Editions::new(false);
        editions.set("projects/p/databases/(default)".to_owned(), true);
        assert!(editions.enterprise("projects/p/databases/(default)"));
        assert!(!editions.enterprise("projects/p/databases/other"));
        assert!(!editions.enterprise("projects/q/databases/(default)"));
        editions.reset();
        assert!(!editions.enterprise("projects/p/databases/(default)"));
        assert!(Editions::new(true).enterprise("projects/p/databases/(default)"));
    }
}
