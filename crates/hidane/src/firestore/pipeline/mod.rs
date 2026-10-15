//! Pipelines: `ExecutePipeline` on an enterprise database (#80), as the official emulator runs
//! them (`tests/fixtures/pipelines.json`, from `tools/oracle/pipelines.py`).
//!
//! A pipeline is read whole first ([`parse`]): its stages, their arguments and options, every
//! expression, with the official errors. It then runs over the database at one read time
//! ([`stages`]), each stage making rows of the rows before it; an `update` or `delete` last
//! commits its writes and answers how many documents it changed. The official emulator
//! returns the rows of a pipeline without `sort` in an order of its own; hidane keeps the
//! order it reads documents in, by name.

mod aggregate;
mod expr;
pub(crate) mod functions;
mod parse;
mod stages;
mod value;

use hidane_core::{field_path::FieldPath, path::ResourcePath};
use hidane_proto::google::firestore::v1::{
    Document, ExecutePipelineRequest, ExecutePipelineResponse, Precondition, Write,
    execute_pipeline_request, precondition, write,
};
use tonic::Status;

use self::expr::{Env, Expr, Row};
use super::{Consistency, FirestoreService, names::Name};

/// A pipeline, read.
#[derive(Debug, Clone)]
pub(crate) struct Pipeline {
    pub(crate) stages: Vec<Stage>,
}

#[derive(Debug, Clone)]
pub(crate) enum Stage {
    Collection(ResourcePath),
    /// A subcollection of the document of the row a subpipeline is evaluated for.
    Subcollection(String),
    CollectionGroup(String),
    Database,
    Documents(Vec<ResourcePath>),
    Where(Expr),
    /// The fields to make, and whether the rows keep their document's name
    /// (`{__name__: __name__}`).
    Select(Vec<(FieldPath, Expr)>, bool),
    AddFields(Vec<(FieldPath, Expr)>),
    RemoveFields(Vec<FieldPath>),
    ReplaceWith(Expr, Mode),
    Sort(Vec<(Expr, bool)>),
    Limit(i64),
    Offset(i64),
    Aggregate {
        accumulators: Vec<(String, aggregate::Accumulator)>,
        groups: Vec<(String, Expr)>,
    },
    Distinct(Vec<(String, Expr)>),
    FindNearest {
        field: Expr,
        vector: Vec<f64>,
        measure: Measure,
        limit: Option<i64>,
        distance_field: Option<FieldPath>,
    },
    Sample(Sample),
    Union(Box<Pipeline>),
    Unnest {
        expr: Expr,
        alias: FieldPath,
        index: Option<FieldPath>,
    },
    Let(Vec<(String, Expr)>),
    Update(Vec<(FieldPath, Expr)>),
    Delete,
}

impl Stage {
    fn is_input(&self) -> bool {
        matches!(
            self,
            Self::Collection(_)
                | Self::Subcollection(_)
                | Self::CollectionGroup(_)
                | Self::Database
                | Self::Documents(_)
        )
    }

    fn is_dml(&self) -> bool {
        matches!(self, Self::Update(_) | Self::Delete)
    }

    /// The name of a stage whose rows have no document name.
    fn drops_names(&self) -> Option<&'static str> {
        match self {
            Self::Select(_, false) => Some("select"),
            Self::Aggregate { .. } => Some("aggregate"),
            Self::Distinct(_) => Some("distinct"),
            Self::ReplaceWith(_, Mode::FullReplace) => Some("replace_with"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    FullReplace,
    MergeOverwrite,
    MergeKeep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Measure {
    Euclidean,
    Cosine,
    DotProduct,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Sample {
    Documents(usize),
    Percent(f64),
}

impl FirestoreService {
    /// `ExecutePipeline` on `database`, an enterprise one, for a caller already admitted.
    pub(crate) fn execute(
        &self,
        database: &str,
        req: &ExecutePipelineRequest,
    ) -> Result<Vec<ExecutePipelineResponse>, Status> {
        // A request without a pipeline is an `UNKNOWN` error without a message, as one
        // without stages.
        let Some(execute_pipeline_request::PipelineType::StructuredPipeline(structured)) =
            &req.pipeline_type
        else {
            return Err(Status::unknown(""));
        };
        let proto = structured.pipeline.clone().unwrap_or_default();
        let pipeline = parse::pipeline(&proto, false)?;
        let consistency = match &req.consistency_selector {
            Some(execute_pipeline_request::ConsistencySelector::Transaction(t)) => {
                Consistency::Transaction(t)
            }
            Some(execute_pipeline_request::ConsistencySelector::NewTransaction(options)) => {
                Consistency::NewTransaction(options)
            }
            Some(execute_pipeline_request::ConsistencySelector::ReadTime(ts)) => {
                Consistency::ReadTime(ts)
            }
            None => Consistency::Latest,
        };
        let (at, transaction) = self.query_read_time(database, consistency, None)?;
        // The transaction an `update` or `delete` writes in: the request's, or the one it began.
        let writes_in = match &req.consistency_selector {
            Some(execute_pipeline_request::ConsistencySelector::Transaction(t)) => Some(t.clone()),
            _ => transaction.clone(),
        };
        let env = Env {
            store: self.store.as_ref(),
            database,
            at,
        };
        let rows = stages::run(&pipeline, &env, &[], None)?;
        let dml = pipeline.stages.last().filter(|s| s.is_dml());
        let (rows, execution_time) = match dml {
            Some(stage) => (
                vec![self.commit_dml(database, stage, &rows, writes_in.as_deref())?],
                None,
            ),
            None => (rows, Some(at.to_timestamp())),
        };
        Ok(vec![ExecutePipelineResponse {
            transaction: transaction.unwrap_or_default(),
            results: rows.iter().map(|row| result(database, row)).collect(),
            execution_time,
            explain_stats: None,
        }])
    }

    /// Writes what an `update` or `delete` stage changes, in one commit, or in `transaction`,
    /// whose commit writes it (`tests/fixtures/pipelines.json`): the row of how many documents
    /// it changed.
    fn commit_dml(
        &self,
        database: &str,
        stage: &Stage,
        rows: &[Row],
        transaction: Option<&[u8]>,
    ) -> Result<Row, Status> {
        let writes: Vec<Write> = rows
            .iter()
            .filter_map(|row| {
                let doc = row.doc.as_ref()?;
                let name = Name::document_name(database, &doc.path);
                Some(match stage {
                    // The row as the stages before left it replaces the document: fields
                    // they added or removed are written too.
                    Stage::Update(_) => Write {
                        operation: Some(write::Operation::Update(Document {
                            name,
                            fields: row.fields.clone(),
                            ..Document::default()
                        })),
                        current_document: Some(Precondition {
                            condition_type: Some(precondition::ConditionType::Exists(true)),
                        }),
                        ..Write::default()
                    },
                    _ => Write {
                        operation: Some(write::Operation::Delete(name)),
                        ..Write::default()
                    },
                })
            })
            .collect();
        let count = writes.len();
        match transaction {
            Some(transaction) => {
                let documents: Vec<ResourcePath> = rows
                    .iter()
                    .filter_map(|row| row.doc.as_ref().map(|doc| (*doc.path).clone()))
                    .collect();
                self.transactions
                    .buffer(database, transaction, &documents, writes)?;
            }
            None if writes.is_empty() => {}
            None => {
                self.commit_writes(database, &writes, None)?;
            }
        }
        let mut fields = hidane_core::field_path::Fields::new();
        fields.insert(
            "documents_modified".to_owned(),
            value::int(i64::try_from(count).unwrap_or(i64::MAX)),
        );
        Ok(Row {
            doc: None,
            fields,
            vars: Vec::new(),
        })
    }
}

/// A row as a result: a document's name and times when it has them.
fn result(database: &str, row: &Row) -> Document {
    match &row.doc {
        Some(doc) => Document {
            name: Name::document_name(database, &doc.path),
            fields: row.fields.clone(),
            create_time: doc.times.map(|(created, _)| created),
            update_time: doc.times.map(|(_, updated)| updated),
        },
        None => Document {
            fields: row.fields.clone(),
            ..Document::default()
        },
    }
}
