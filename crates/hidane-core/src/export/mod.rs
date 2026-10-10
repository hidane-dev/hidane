//! The official emulator's export format (Cloud Datastore's managed export), byte for byte.
//!
//! An export named `N` is a directory `N/` holding:
//!
//! - `N.overall_export_metadata`: a [`log`] of two records, the version byte `0x33` and an
//!   `OverallExportMetadata` message pointing at each kind directory's metadata, with its
//!   entity count and output size; an empty export has no pointer and no kind directory;
//! - `all_namespaces/all_kinds/all_namespaces_all_kinds.export_metadata`: a plain message
//!   with the export's name, start and end time (microseconds), and its output files;
//! - `all_namespaces/all_kinds/output-0`: a [`log`] with one [`entity`] per document.
//!
//! `docs/export-format.md` describes every field; `tests/fixtures/export_import.json` in the
//! `hidane` crate holds exports written by the official emulator.

pub mod entity;
pub mod log;
mod wire;

use std::collections::BTreeMap;

use hidane_proto::google::firestore::v1::{Value, value::ValueType};

pub use wire::Malformed;

use crate::path::ResourcePath;
use wire::{Reader, Writer};

/// The first record of an overall metadata file.
pub const VERSION: u8 = 0x33;

/// Where the one kind directory's metadata sits, relative to the export directory.
pub const KIND_METADATA: &str = "all_namespaces/all_kinds/all_namespaces_all_kinds.export_metadata";

/// The one output file, relative to the kind directory.
pub const OUTPUT: &str = "output-0";

/// A document as an export holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedDocument {
    /// The database the key names, `(default)` when it names none.
    pub database_id: String,
    pub path: ResourcePath,
    /// References relative to the importing project have an empty project
    /// (`projects//databases/…`); see [`Self::fields_in`].
    pub fields: BTreeMap<String, Value>,
}

impl ExportedDocument {
    /// The fields as imported into `project`: references relative to the importing project
    /// (top-level indexed properties, see [`entity`]) point into it.
    pub fn fields_in(&self, project: &str) -> BTreeMap<String, Value> {
        fn resolve(value: &mut Value, project: &str) {
            match &mut value.value_type {
                Some(ValueType::ReferenceValue(name)) => {
                    if let Some(rest) = name.strip_prefix("projects//") {
                        *name = format!("projects/{project}/{rest}");
                    }
                }
                Some(ValueType::ArrayValue(array)) => {
                    array.values.iter_mut().for_each(|v| resolve(v, project));
                }
                _ => {}
            }
        }
        let mut fields = self.fields.clone();
        fields.values_mut().for_each(|v| resolve(v, project));
        fields
    }
}

/// An `OverallExportMetadata` pointer to a kind directory's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    /// Relative to the export directory.
    pub path: String,
    pub entities: u64,
    /// The size of the kind's output files.
    pub bytes: u64,
}

/// The contents of `{name}.overall_export_metadata`.
pub fn overall_metadata(pointer: Option<&Pointer>) -> Vec<u8> {
    let mut w = Writer::default();
    if let Some(pointer) = pointer {
        w.message(1, |p| {
            // Constant in every export the official emulator writes.
            p.message(1, |format| {
                format.uint(1, 2);
                format.uint(3, 3);
            });
            p.bytes(2, pointer.path.as_bytes());
            p.uint(3, pointer.entities);
            p.uint(4, pointer.bytes);
        });
    }
    log::write_all([&[VERSION][..], &w.into_bytes()])
}

/// Why an overall metadata file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverallError {
    /// The first record is missing or is not [`VERSION`].
    Version,
    Malformed,
}

/// The kind metadata paths an overall metadata file points at, relative to its directory.
pub fn parse_overall_metadata(data: &[u8]) -> Result<Vec<String>, OverallError> {
    let mut records = log::records(data);
    match records.next() {
        Some(Ok(version)) if *version == [VERSION] => {}
        _ => return Err(OverallError::Version),
    }
    let Some(Ok(metadata)) = records.next() else {
        return Err(OverallError::Malformed);
    };
    let mut paths = Vec::new();
    for field in Reader::new(&metadata) {
        let (number, pointer) = field.map_err(|_| OverallError::Malformed)?;
        if number != 1 {
            continue;
        }
        for field in pointer.fields().ok_or(OverallError::Malformed)? {
            let (number, value) = field.map_err(|_| OverallError::Malformed)?;
            if number == 2 {
                let path = value.as_bytes().ok_or(OverallError::Malformed)?;
                paths.push(String::from_utf8(path.to_vec()).map_err(|_| OverallError::Malformed)?);
            }
        }
    }
    Ok(paths)
}

/// The contents of a kind directory's `.export_metadata`.
pub fn export_metadata(name: &str, start_micros: i64, end_micros: i64) -> Vec<u8> {
    let mut w = Writer::default();
    w.message(1, |m| {
        m.bytes(1, name.as_bytes());
        m.int(2, start_micros);
        m.int(3, end_micros);
    });
    w.message(2, |files| {
        files.bytes(1, b"");
        files.bytes(2, OUTPUT.as_bytes());
    });
    w.into_bytes()
}

/// The output files a kind's metadata lists, relative to its directory.
pub fn parse_export_metadata(data: &[u8]) -> Result<Vec<String>, Malformed> {
    let mut outputs = Vec::new();
    for field in Reader::new(data) {
        let (number, files) = field?;
        if number != 2 {
            continue;
        }
        let mut prefix = String::new();
        for field in files.fields().ok_or(Malformed)? {
            let (number, value) = field?;
            let text = String::from_utf8(value.as_bytes().ok_or(Malformed)?.to_vec())
                .map_err(|_| Malformed)?;
            match number {
                1 => prefix = text,
                2 if prefix.is_empty() => outputs.push(text),
                2 => outputs.push(format!("{}/{text}", prefix.trim_end_matches('/'))),
                _ => {}
            }
        }
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_export_points_at_nothing() {
        let data = overall_metadata(None);
        assert_eq!(parse_overall_metadata(&data), Ok(vec![]));
        assert_eq!(data.len(), 15);
    }

    #[test]
    fn metadata_round_trips() {
        let pointer = Pointer {
            path: KIND_METADATA.to_owned(),
            entities: 9,
            bytes: 4928,
        };
        assert_eq!(
            parse_overall_metadata(&overall_metadata(Some(&pointer))),
            Ok(vec![KIND_METADATA.to_owned()])
        );
        assert_eq!(
            parse_export_metadata(&export_metadata("n", 1, 2)),
            Ok(vec![OUTPUT.to_owned()])
        );
    }

    #[test]
    fn another_version_is_refused() {
        assert_eq!(
            parse_overall_metadata(&log::write_all([&[0x34u8][..], &[]])),
            Err(OverallError::Version)
        );
        assert_eq!(
            parse_overall_metadata(b"garbage"),
            Err(OverallError::Version)
        );
    }
}
