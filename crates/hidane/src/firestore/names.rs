//! Resource names: `projects/{project}/databases/{database}/documents/{path}`.
//!
//! Error messages are the official emulator's (`tests/fixtures/document_writes.json`), which
//! point at the offending byte offset.

use hidane_core::{order::numeric_id, path::ResourcePath};
use tonic::Status;

/// A parsed name: the database (`projects/p/databases/d`) and the path below `documents`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub database: String,
    pub path: ResourcePath,
}

impl Name {
    pub fn document_name(database: &str, path: &ResourcePath) -> String {
        if path.is_empty() {
            format!("{database}/documents")
        } else {
            format!("{database}/documents/{path}")
        }
    }
}

/// Parses `projects/{p}/databases/{d}`.
pub fn database(name: &str) -> Result<String, Status> {
    let mut cursor = Cursor::new(name, "Database name");
    cursor.literal("projects")?;
    cursor.literal("/")?;
    cursor.segment("project")?;
    cursor.literal("/")?;
    cursor.literal("databases")?;
    cursor.literal("/")?;
    cursor.segment("database")?;
    cursor.end()?;
    Ok(name.to_owned())
}

/// Parses a document name (an even number of path segments).
pub fn document(name: &str) -> Result<Name, Status> {
    parse(name, Shape::Document)
}

/// Parses a parent: the database root (`…/documents`) or a document.
pub fn parent(name: &str) -> Result<Name, Status> {
    parse(name, Shape::Parent)
}

/// Parses the name of a document or a collection (any number of path segments).
pub fn any(name: &str) -> Result<Name, Status> {
    parse(name, Shape::Any)
}

/// Validates the collection ID of a query's `from`. The empty ID (a query over every
/// collection) is the caller's business.
pub fn query_collection_id(id: &str) -> Result<(), Status> {
    let problem = if id.contains('/') {
        "contains \"/\""
    } else if reserved_pattern(id) {
        "is reserved"
    } else {
        return Ok(());
    };
    Err(Status::invalid_argument(format!(
        "Collection id \"{id}\" is invalid because it {problem}."
    )))
}

/// Validates a collection ID given on its own (`ListDocuments`, `CreateDocument`). The empty
/// ID is the caller's business.
pub fn collection_id(id: &str) -> Result<(), Status> {
    if id == "." || id == ".." {
        return Err(Status::invalid_argument(format!(
            "Collection id \"{id}\" is invalid because it is reserved."
        )));
    }
    query_collection_id(id)
}

/// Validates a document ID given on its own (`CreateDocument`).
pub fn document_id(id: &str) -> Result<(), Status> {
    let problem = if id.contains('/') {
        "contains \"/\""
    } else if id == "." || id == ".." {
        "is reserved"
    } else {
        return reserved(id);
    };
    Err(Status::invalid_argument(format!(
        "Resource id \"{id}\" is invalid because it {problem}."
    )))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Document,
    Parent,
    Any,
}

fn parse(name: &str, shape: Shape) -> Result<Name, Status> {
    let what = match shape {
        Shape::Document => "Document name",
        Shape::Parent => "Document parent name",
        Shape::Any => "Resource name",
    };
    let mut cursor = Cursor::new(name, what);
    cursor.literal("projects")?;
    cursor.literal("/")?;
    cursor.segment("project")?;
    cursor.literal("/")?;
    cursor.literal("databases")?;
    cursor.literal("/")?;
    cursor.segment("database")?;
    let database_end = cursor.pos;
    cursor.literal("/")?;
    cursor.literal("documents")?;

    let mut segments = Vec::new();
    loop {
        if cursor.at_end() {
            // The root is a valid parent; a document needs at least one collection/ID pair.
            let complete = segments.len() % 2 == 0;
            let allowed = match shape {
                Shape::Document => complete && !segments.is_empty(),
                Shape::Parent => complete,
                Shape::Any => true,
            };
            if !allowed {
                return Err(cursor.lacks("/"));
            }
            break;
        }
        cursor.literal("/")?;
        if segments.is_empty() && cursor.at_end() {
            return Err(Status::invalid_argument(format!(
                "{what} \"{name}\" is invalid: Omit trailing \"/\"."
            )));
        }
        let start = cursor.pos;
        let kind = if segments.len() % 2 == 0 {
            "collection"
        } else {
            "resource"
        };
        let segment = cursor.segment(kind)?;
        if segment == "." || segment == ".." {
            return Err(Status::invalid_argument(format!(
                "{what} \"{name}\" contains a resource id \"{segment}\" at index {start}."
            )));
        }
        // Segments alternate between collection IDs and document IDs, which are reserved
        // differently.
        if segments.len() % 2 == 0 {
            if reserved_pattern(segment) {
                return Err(Status::invalid_argument(format!(
                    "Collection id \"{segment}\" is invalid because it is reserved."
                )));
            }
        } else {
            reserved(segment)?;
        }
        segments.push(segment.to_owned());
    }
    Ok(Name {
        database: name[..database_end].to_owned(),
        path: ResourcePath::from_segments(segments),
    })
}

fn reserved_pattern(id: &str) -> bool {
    id.len() >= 4 && id.starts_with("__") && id.ends_with("__")
}

/// Document IDs matching `__.*__` are reserved, except well-formed numeric IDs
/// (`__id<i64>__`); collection IDs have no such exception.
fn reserved(id: &str) -> Result<(), Status> {
    if reserved_pattern(id) {
        if id.starts_with("__id") {
            match numeric_id(id) {
                None => {
                    return Err(Status::invalid_argument(format!(
                        "A long document ID string must be formatted like '__id{{LONG}}__', \
                         where LONG is a 64-bit integer. Found: '{id}'."
                    )));
                }
                Some(0) => {
                    return Err(Status::invalid_argument(
                        "Document ID of type long must be non-zero",
                    ));
                }
                Some(_) => {}
            }
        } else {
            return Err(Status::invalid_argument(format!(
                "Resource id \"{id}\" is invalid because it is reserved."
            )));
        }
    }
    Ok(())
}

struct Cursor<'a> {
    name: &'a str,
    what: &'static str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(name: &'a str, what: &'static str) -> Self {
        Self { name, what, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos == self.name.len()
    }

    fn lacks(&self, expected: &str) -> Status {
        Status::invalid_argument(format!(
            "{} \"{}\" lacks \"{expected}\" at index {}.",
            self.what, self.name, self.pos
        ))
    }

    fn literal(&mut self, expected: &str) -> Result<(), Status> {
        if self.name[self.pos..].starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(self.lacks(expected))
        }
    }

    /// The next segment, a `kind` ID (`project`, `database`, `collection`, `resource`).
    fn segment(&mut self, kind: &str) -> Result<&'a str, Status> {
        let rest = &self.name[self.pos..];
        let len = rest.find('/').unwrap_or(rest.len());
        if len == 0 {
            return Err(if rest.is_empty() {
                self.trailing_slash()
            } else {
                Status::invalid_argument(format!(
                    "{} \"{}\" lacks a {kind} id at index {}.",
                    self.what, self.name, self.pos
                ))
            });
        }
        self.pos += len;
        Ok(&rest[..len])
    }

    fn trailing_slash(&self) -> Status {
        Status::invalid_argument(format!(
            "{} \"{}\" has invalid trailing \"/\".",
            self.what, self.name
        ))
    }

    fn end(&self) -> Result<(), Status> {
        if self.at_end() {
            Ok(())
        } else if &self.name[self.pos..] == "/" {
            Err(self.trailing_slash())
        } else {
            Err(Status::invalid_argument(format!(
                "{} \"{}\" has unexpected trailing characters at index {}.",
                self.what, self.name, self.pos
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(r: Result<Name, Status>) -> String {
        r.unwrap_err().message().to_owned()
    }

    #[test]
    fn parses_documents_and_parents() {
        let n = document("projects/p/databases/(default)/documents/c/d/sub/x").unwrap();
        assert_eq!(n.database, "projects/p/databases/(default)");
        assert_eq!(n.path.to_string(), "c/d/sub/x");
        let root = parent("projects/p/databases/(default)/documents").unwrap();
        assert!(root.path.is_empty());
        assert_eq!(
            parent("projects/p/databases/d/documents/c/d")
                .unwrap()
                .path
                .len(),
            2
        );
        assert!(document("projects/p/databases/d/documents/c/__id5__").is_ok());
    }

    #[test]
    fn errors_match_the_official_emulator() {
        assert_eq!(
            err(document("projects/p-grpc/databases/(default)/documents/c")),
            "Document name \"projects/p-grpc/databases/(default)/documents/c\" lacks \"/\" at index 47."
        );
        assert_eq!(
            err(document("projects/p-grpc/databases/(default)/c/d")),
            "Document name \"projects/p-grpc/databases/(default)/c/d\" lacks \"documents\" at index 36."
        );
        assert_eq!(
            err(document(
                "projects/p-dot-id/databases/(default)/documents/c/."
            )),
            "Document name \"projects/p-dot-id/databases/(default)/documents/c/.\" contains a resource id \".\" at index 50."
        );
        assert_eq!(
            err(document("projects/p/databases/d/documents//a")),
            "Document name \"projects/p/databases/d/documents//a\" lacks a collection id at index 33."
        );
        assert_eq!(
            err(document("projects/p/databases/d/documents/c//")),
            "Document name \"projects/p/databases/d/documents/c//\" lacks a resource id at index 35."
        );
        assert_eq!(
            err(document("projects/p/databases/d/documents/c/a/")),
            "Document name \"projects/p/databases/d/documents/c/a/\" has invalid trailing \"/\"."
        );
        assert_eq!(
            err(parent("projects/p/databases/d/documents/")),
            "Document parent name \"projects/p/databases/d/documents/\" is invalid: Omit trailing \"/\"."
        );
        assert_eq!(
            database("projects//databases/d").unwrap_err().message(),
            "Database name \"projects//databases/d\" lacks a project id at index 9."
        );
        assert_eq!(
            database("projects/p/databases/d/").unwrap_err().message(),
            "Database name \"projects/p/databases/d/\" has invalid trailing \"/\"."
        );
        assert_eq!(
            err(parent("projects/pn/databases/(default)/documents/p")),
            "Document parent name \"projects/pn/databases/(default)/documents/p\" lacks \"/\" at index 43."
        );
        assert_eq!(
            query_collection_id("a/b").unwrap_err().message(),
            "Collection id \"a/b\" is invalid because it contains \"/\"."
        );
        assert_eq!(
            query_collection_id("__x__").unwrap_err().message(),
            "Collection id \"__x__\" is invalid because it is reserved."
        );
        assert_eq!(
            err(document("projects/p/databases/(default)/documents/c/__x__")),
            "Resource id \"__x__\" is invalid because it is reserved."
        );
        assert_eq!(
            err(document(
                "projects/p/databases/(default)/documents/c/__id5x__"
            )),
            "A long document ID string must be formatted like '__id{LONG}__', where LONG is a \
             64-bit integer. Found: '__id5x__'."
        );
    }
}
