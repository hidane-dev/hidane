//! `GET /emulator/v1/projects/{p}:indexUsage?database=…`: the composite indexes the queries run
//! on a database would need in production, with how many queries used each, as the official
//! emulator keeps them (`tests/fixtures/index_usage.json`, from `tools/oracle/index_usage.py`,
//! #36).
//!
//! Every RunQuery, RunAggregationQuery and listener's query that runs (not one a check refuses)
//! counts once for each of its conjunctions in disjunctive normal form
//! ([`super::query::Query::needed_indexes`]). `POST /reset` forgets them. The official emulator
//! lists the reports in an order of its own; hidane lists them in the order first needed.

use std::{collections::HashMap, fmt::Write as _, sync::Mutex};

use crate::rest::json_string;

/// How an index holds a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ascending,
    Descending,
    Contains,
}

/// A composite index: the collection it is on, whether for a collection group, and its fields
/// in order, `__name__` last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub collection_id: String,
    pub group: bool,
    pub fields: Vec<(String, Kind)>,
}

impl Index {
    /// The index as protobuf-java's JsonFormat prints it, at `indent`.
    fn print(&self, database: &str, indent: &str, out: &mut String) {
        let name = format!(
            "{database}/collectionGroups/{}/indexes/_",
            self.collection_id
        );
        let scope = if self.group {
            "COLLECTION_GROUP"
        } else {
            "COLLECTION"
        };
        let _ = write!(
            out,
            "{{\n{indent}  \"name\": {},\n{indent}  \"queryScope\": \"{scope}\",\n{indent}  \"fields\": [",
            json_string(&name)
        );
        for (i, (path, kind)) in self.fields.iter().enumerate() {
            let (key, value) = match kind {
                Kind::Contains => ("arrayConfig", "CONTAINS"),
                Kind::Ascending => ("order", "ASCENDING"),
                Kind::Descending => ("order", "DESCENDING"),
            };
            let _ = write!(
                out,
                "{}{{\n{indent}    \"fieldPath\": {},\n{indent}    \"{key}\": \"{value}\"\n{indent}  }}",
                if i == 0 { "" } else { ", " },
                json_string(path)
            );
        }
        let _ = write!(out, "]\n{indent}}}");
    }
}

/// The indexes each database's queries needed, in the order first needed, with their counts.
#[derive(Default)]
pub struct IndexUsage {
    databases: Mutex<HashMap<String, Vec<(Index, u64)>>>,
}

impl IndexUsage {
    /// Counts a query on `database` that needed `indexes`, one per conjunction.
    pub fn record(&self, database: &str, indexes: Vec<Index>) {
        if indexes.is_empty() {
            return;
        }
        let mut databases = self.databases.lock().unwrap_or_else(|e| e.into_inner());
        let reports = databases.entry(database.to_owned()).or_default();
        for index in indexes {
            match reports.iter_mut().find(|(seen, _)| *seen == index) {
                Some((_, count)) => *count += 1,
                None => reports.push((index, 1)),
            }
        }
    }

    /// The body `:indexUsage` answers for `database`, as protobuf-java's JsonFormat prints it:
    /// `{}` without a report.
    pub fn report(&self, database: &str) -> String {
        let databases = self.databases.lock().unwrap_or_else(|e| e.into_inner());
        let Some(reports) = databases.get(database) else {
            return "{\n}\n".to_owned();
        };
        let mut out = "{\n  \"reports\": [".to_owned();
        for (i, (index, count)) in reports.iter().enumerate() {
            out.push_str(if i == 0 {
                "{\n    \"index\": "
            } else {
                ", {\n    \"index\": "
            });
            index.print(database, "    ", &mut out);
            let _ = write!(out, ",\n    \"numQueries\": \"{count}\"\n  }}");
        }
        out.push_str("]\n}\n");
        out
    }

    /// `POST /reset`.
    pub fn clear(&self) {
        self.databases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
