//! Expressions and their evaluation: a value, absent (`None`: a field a document does not
//! have, or what a function gives for one), or an error, which fails the whole request.

use std::sync::Arc;

use hidane_core::{
    field_path::{FieldPath, Fields},
    order::is_vector,
    path::ResourcePath,
    store::{ReadTime, Store},
};
use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};
use prost_types::Timestamp;
use tonic::Status;

use super::{Pipeline, functions::Def, stages, value};
use crate::firestore::names::Name;

/// What an expression evaluates to: a value, or absent.
pub(crate) type Eval = Result<Option<Value>, Status>;

#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Constant(Value),
    Field(Field),
    Variable(String),
    Call(Box<Call>),
    /// A pipeline as an argument (`scalar(...)`, `array(...)`).
    Pipeline(Box<Pipeline>),
    /// An array or map with expressions among its values.
    Array(Vec<Expr>),
    Map(Vec<(String, Expr)>),
}

/// A field reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Field {
    /// `__name__`: the document's reference.
    Name,
    CreateTime,
    UpdateTime,
    Path(FieldPath),
}

impl Field {
    pub(crate) fn parse(path: &str) -> Result<Self, Status> {
        Ok(match path {
            "__name__" => Self::Name,
            "__create_time__" => Self::CreateTime,
            "__update_time__" => Self::UpdateTime,
            _ => Self::Path(FieldPath::parse(path).map_err(Status::invalid_argument)?),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub(crate) def: &'static Def,
    pub(crate) args: Vec<Expr>,
}

impl Expr {
    /// The string a literal argument holds.
    pub(crate) fn literal_string(&self) -> Option<&str> {
        match self {
            Self::Constant(Value {
                value_type: Some(ValueType::StringValue(s)),
            }) => Some(s),
            _ => None,
        }
    }
}

/// The document a row came from: its path, and its create and update times, which a `select`
/// that keeps `__name__` drops.
#[derive(Debug, Clone)]
pub(crate) struct Meta {
    pub(crate) path: Arc<ResourcePath>,
    pub(crate) times: Option<(Timestamp, Timestamp)>,
}

/// A row of a pipeline: a document's fields, or what stages made of them, with the variables
/// `let` defined for it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Row {
    pub(crate) doc: Option<Meta>,
    pub(crate) fields: Fields,
    pub(crate) vars: Vec<(String, Value)>,
}

/// What every evaluation reads: the store at the pipeline's read time.
pub(crate) struct Env<'a> {
    pub(crate) store: &'a dyn Store,
    pub(crate) database: &'a str,
    pub(crate) at: ReadTime,
}

/// A variable a function binds while it evaluates (`array_filter`'s element).
pub(crate) struct Local<'a> {
    pub(crate) name: &'a str,
    pub(crate) value: Value,
    pub(crate) parent: Option<&'a Local<'a>>,
}

/// Where an expression is evaluated: a row, the variables bound around it, the store.
#[derive(Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub(crate) row: &'a Row,
    pub(crate) locals: Option<&'a Local<'a>>,
    pub(crate) env: &'a Env<'a>,
}

impl<'a> Scope<'a> {
    pub(crate) fn new(row: &'a Row, env: &'a Env<'a>) -> Self {
        Self {
            row,
            locals: None,
            env,
        }
    }

    pub(crate) fn eval(&self, expr: &Expr) -> Eval {
        match expr {
            Expr::Constant(v) => Ok(Some(v.clone())),
            Expr::Field(field) => Ok(self.field(field)),
            Expr::Variable(name) => self.variable(name).map(Some),
            Expr::Call(call) => (call.def.eval)(call, self),
            Expr::Pipeline(_) => Err(Status::invalid_argument(
                "A pipeline can only be an argument of scalar(...) or array(...).",
            )),
            Expr::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item)?.unwrap_or_else(value::null));
                }
                Ok(Some(value::array(values)))
            }
            Expr::Map(entries) => {
                let mut fields = Fields::new();
                for (key, item) in entries {
                    if let Some(v) = self.eval(item)? {
                        fields.insert(key.clone(), v);
                    }
                }
                Ok(Some(value::map(fields)))
            }
        }
    }

    /// The value of a field of the row, if it has it.
    pub(crate) fn field(&self, field: &Field) -> Option<Value> {
        field_of(self.row, field, self.env.database)
    }

    /// The value of a variable: the innermost binding, then the row's.
    pub(crate) fn variable(&self, name: &str) -> Result<Value, Status> {
        let mut local = self.locals;
        while let Some(l) = local {
            if l.name == name {
                return Ok(l.value.clone());
            }
            local = l.parent;
        }
        self.row
            .vars
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| {
                Status::invalid_argument(format!(
                    "The variable '{name}' was either never defined, or was attempted to be \
                     accessed from a scope where it is not visible."
                ))
            })
    }

    /// Every variable visible here, innermost last: what a subpipeline sees.
    pub(crate) fn visible_variables(&self) -> Vec<(String, Value)> {
        let mut vars = self.row.vars.clone();
        let mut locals = Vec::new();
        let mut local = self.locals;
        while let Some(l) = local {
            locals.push((l.name.to_owned(), l.value.clone()));
            local = l.parent;
        }
        vars.extend(locals.into_iter().rev());
        vars
    }

    /// Evaluates `expr` with `name` bound to `value`.
    pub(crate) fn with_local(&self, name: &str, value: Value, expr: &Expr) -> Eval {
        let local = Local {
            name,
            value,
            parent: self.locals,
        };
        Scope {
            row: self.row,
            locals: Some(&local),
            env: self.env,
        }
        .eval(expr)
    }

    /// Evaluates `expr` with two bindings.
    pub(crate) fn with_locals(
        &self,
        (first, first_value): (&str, Value),
        (second, second_value): (&str, Value),
        expr: &Expr,
    ) -> Eval {
        let outer = Local {
            name: first,
            value: first_value,
            parent: self.locals,
        };
        let inner = Local {
            name: second,
            value: second_value,
            parent: Some(&outer),
        };
        Scope {
            row: self.row,
            locals: Some(&inner),
            env: self.env,
        }
        .eval(expr)
    }

    /// Runs a subpipeline, which sees the variables visible here, and whose `subcollection`
    /// reads below this row's document.
    pub(crate) fn run(&self, pipeline: &Pipeline) -> Result<Vec<Row>, Status> {
        let document = self.row.doc.as_ref().map(|doc| doc.path.as_ref());
        stages::run(pipeline, self.env, &self.visible_variables(), document)
    }
}

/// The value of `field` in `row`, if it has it.
pub(crate) fn field_of(row: &Row, field: &Field, database: &str) -> Option<Value> {
    match field {
        Field::Name => row.doc.as_ref().map(|doc| {
            value::of(ValueType::ReferenceValue(Name::document_name(
                database, &doc.path,
            )))
        }),
        Field::CreateTime => row
            .doc
            .as_ref()
            .and_then(|doc| doc.times)
            .map(|(created, _)| value::timestamp(created)),
        Field::UpdateTime => row
            .doc
            .as_ref()
            .and_then(|doc| doc.times)
            .map(|(_, updated)| value::timestamp(updated)),
        Field::Path(path) => get(&row.fields, path).cloned(),
    }
}

/// The value at `path` in `fields`: each segment but the last names a field of a map.
pub(crate) fn get<'a>(fields: &'a Fields, path: &FieldPath) -> Option<&'a Value> {
    let (last, parents) = path.segments().split_last()?;
    let mut current = fields;
    for segment in parents {
        current = match current.get(segment)?.value_type.as_ref()? {
            ValueType::MapValue(map) if !is_vector(map) => &map.fields,
            _ => return None,
        };
    }
    current.get(last)
}

/// Sets `path` in `fields` to `value`, making maps on the way and replacing what is not one.
pub(crate) fn set(fields: &mut Fields, path: &FieldPath, value: Value) {
    let Some((last, parents)) = path.segments().split_last() else {
        return;
    };
    let mut current = fields;
    for segment in parents {
        let entry = current
            .entry(segment.clone())
            .or_insert_with(|| value::map(Fields::new()));
        if !matches!(&entry.value_type, Some(ValueType::MapValue(map)) if !is_vector(map)) {
            *entry = value::map(Fields::new());
        }
        let Some(ValueType::MapValue(MapValue { fields: inner })) = &mut entry.value_type else {
            unreachable!("made a map above");
        };
        current = inner;
    }
    current.insert(last.clone(), value);
}

/// Removes `path` from `fields`, if there.
pub(crate) fn remove(fields: &mut Fields, path: &FieldPath) {
    let Some((last, parents)) = path.segments().split_last() else {
        return;
    };
    let mut current = fields;
    for segment in parents {
        current = match current.get_mut(segment).and_then(|v| v.value_type.as_mut()) {
            Some(ValueType::MapValue(map)) => &mut map.fields,
            _ => return,
        };
    }
    current.remove(last);
}
