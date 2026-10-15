//! Reading a pipeline: each stage's arguments and options, and every expression, checked as the
//! official emulator checks them before it reads a document (`tests/fixtures/pipelines.json`).

use hidane_core::{field_path::FieldPath, order::is_vector, path::ResourcePath};
use hidane_proto::google::firestore::v1::{
    Function, Pipeline as Proto, Value, pipeline::Stage as ProtoStage, value::ValueType,
};
use tonic::Status;

use super::{
    Measure, Mode, Pipeline, Sample, Stage,
    aggregate::{self, Accumulator},
    expr::{Call, Expr, Field},
    functions, value,
};

/// What a stage takes, as its errors print it.
struct Spec {
    signature: &'static str,
    min: usize,
    max: Option<usize>,
    options: &'static [&'static str],
}

fn spec(name: &str) -> Option<Spec> {
    let (signature, min, max, options): (&'static str, usize, Option<usize>, &[&str]) = match name {
        "collection" => ("collection(collection_name: Collection)", 1, Some(1), &[]),
        "collection_group" => (
            "collection_group(ancestor: Path, collection_id: String)",
            2,
            Some(2),
            &[],
        ),
        "database" => ("database()", 0, Some(0), &[]),
        "documents" => ("documents(paths: Path*)", 1, None, &[]),
        "where" => ("where(condition: Expr)", 1, Some(1), &[]),
        "select" => ("select(fields: Map[FieldPath, Expr])", 1, Some(1), &[]),
        "add_fields" => ("add_fields(fields: Map[FieldPath, Expr])", 1, Some(1), &[]),
        "remove_fields" => ("remove_fields(fields: FieldReference*)", 1, None, &[]),
        "replace_with" => ("replace_with(map: Expr, mode: Mode)", 2, Some(2), &[]),
        "sort" => ("sort(ordering: Order*)", 1, None, &[]),
        "limit" => ("limit(limit: Int64)", 1, Some(1), &[]),
        "offset" => ("offset(offset: Int64)", 1, Some(1), &[]),
        "aggregate" => (
            "aggregate(accumulators: Map[FieldPath, Expr], groups: Map[FieldPath, Expr])",
            2,
            Some(2),
            &[],
        ),
        "distinct" => ("distinct(fields: Map[FieldPath, Expr])", 1, Some(1), &[]),
        "find_nearest" => (
            "find_nearest(field: FieldReference, vector: Vector, distance_measure: \
             DistanceMeasure, limit: Int64?, distance_field: FieldPath?)",
            3,
            Some(3),
            &["limit", "distance_field"],
        ),
        "sample" => (
            "sample(size: int64 | double, mode: SampleMode)",
            2,
            Some(2),
            &[],
        ),
        "union" => ("union(other: Pipeline)", 1, Some(1), &[]),
        "unnest" => (
            "unnest(field: Expr, alias: FieldReference, index_field: FieldReference?)",
            2,
            Some(2),
            &["index_field"],
        ),
        "let" => ("let(variables: Map[VariableRef, Expr])", 1, Some(1), &[]),
        "update" => ("update(transforms: Map[FieldPath, Expr])", 1, Some(1), &[]),
        "delete" => ("delete()", 0, Some(0), &[]),
        "search" => ("search()", 0, Some(0), &["query"]),
        _ => return None,
    };
    Some(Spec {
        signature,
        min,
        max,
        options,
    })
}

/// Reads a pipeline: its stages, then whether they come in an order it runs.
pub(crate) fn pipeline(proto: &Proto) -> Result<Pipeline, Status> {
    if proto.stages.is_empty() {
        return Err(Status::unknown(""));
    }
    let stages = proto
        .stages
        .iter()
        .map(stage)
        .collect::<Result<Vec<_>, _>>()?;
    if !stages[0].is_input() {
        return Err(Status::invalid_argument(
            "The first stage in a pipeline query must be an input stage.",
        ));
    }
    if stages[1..].iter().any(Stage::is_input) {
        return Err(Status::invalid_argument(
            "The last stage of a pipeline query must be a transform or output stage.",
        ));
    }
    if let Some(at) = stages.iter().position(Stage::is_dml) {
        if at + 1 != stages.len() {
            return Err(Status::invalid_argument(
                "Only transforms are allowed in the middle of a pipeline.",
            ));
        }
        if let Some(name) = stages[..at].iter().find_map(Stage::drops_names) {
            return Err(Status::invalid_argument(format!(
                "The '{name}' stage must have a '__name__' field in a non-transactional DML \
                 context."
            )));
        }
    }
    Ok(Pipeline { stages })
}

fn stage(proto: &ProtoStage) -> Result<Stage, Status> {
    let name = proto.name.as_str();
    let Some(spec) = spec(name) else {
        return Err(Status::invalid_argument(format!(
            "Unsupported stage: {name}"
        )));
    };
    let n = proto.args.len();
    if n < spec.min || spec.max.is_some_and(|max| n > max) {
        let range = match spec.max {
            Some(max) => format!("[{}..{max}]", spec.min),
            None => format!("[{}..+∞)", spec.min),
        };
        return Err(Status::invalid_argument(format!(
            "Stage '{}' takes {range} argument(s), received {n}.",
            spec.signature
        )));
    }
    let unknown: Vec<&str> = proto
        .options
        .keys()
        .map(String::as_str)
        .filter(|o| !spec.options.contains(o))
        .collect();
    if !unknown.is_empty() {
        return Err(Status::invalid_argument(format!(
            "Stage '{}' does not support option(s) [{}].",
            spec.signature,
            unknown.join(", ")
        )));
    }
    let args = &proto.args;
    Ok(match name {
        "collection" => {
            let path = reference("collection_name", &args[0])?;
            Stage::Collection(path_of("Collection name", path, false)?)
        }
        "collection_group" => {
            reference("ancestor", &args[0])?;
            Stage::CollectionGroup(string("collection_id", &args[1])?.to_owned())
        }
        "database" => Stage::Database,
        "documents" => {
            let mut paths: Vec<ResourcePath> = Vec::new();
            for (i, arg) in args.iter().enumerate() {
                let text = reference(&format!("paths[{i}]"), arg)?;
                let path = path_of("Document path name", text, true)?;
                if paths.contains(&path) {
                    return Err(Status::invalid_argument(format!(
                        "Duplicate path '{text}' specified in document stage."
                    )));
                }
                paths.push(path);
            }
            Stage::Documents(paths)
        }
        "where" => Stage::Where(expr(&args[0])?),
        "select" | "add_fields" => {
            let fields = aliases(name, "fields", &args[0])?;
            if name == "select" && fields.is_empty() {
                return Err(Status::invalid_argument(
                    "The 'select(...)' stage requires at least one field.",
                ));
            }
            if name == "select" {
                Stage::Select(fields)
            } else {
                Stage::AddFields(fields)
            }
        }
        "remove_fields" => Stage::RemoveFields(
            args.iter()
                .enumerate()
                .map(|(i, arg)| field_path(&format!("fields[{i}]"), arg))
                .collect::<Result<_, _>>()?,
        ),
        "replace_with" => {
            let map = expr(&args[0])?;
            let mode = match string("mode", &args[1])? {
                "full_replace" => Mode::FullReplace,
                "merge_overwrite_existing" => Mode::MergeOverwrite,
                "merge_keep_existing" => Mode::MergeKeep,
                other => {
                    return Err(Status::invalid_argument(format!(
                        "Unsupported mode '{other}'. Valid options are: [full_replace, \
                         merge_overwrite_existing, merge_keep_existing]."
                    )));
                }
            };
            Stage::ReplaceWith(map, mode)
        }
        "sort" => Stage::Sort(
            args.iter()
                .enumerate()
                .map(|(i, arg)| ordering(i, arg))
                .collect::<Result<_, _>>()?,
        ),
        "limit" | "offset" => {
            let n = integer(name, &args[0])?;
            if n < 0 {
                return Err(Status::invalid_argument(format!(
                    "The '{name}(...)' cannot have a negative value: {n}"
                )));
            }
            if name == "limit" {
                Stage::Limit(n)
            } else {
                Stage::Offset(n)
            }
        }
        "aggregate" => aggregate_stage(&args[0], &args[1])?,
        "distinct" => {
            let fields = map_of("fields", &args[0])?;
            if fields.is_empty() {
                return Err(Status::invalid_argument(
                    "The 'distinct(...)' stage requires at least one field.",
                ));
            }
            Stage::Distinct(
                fields
                    .iter()
                    .map(|(alias, v)| Ok((top_level(alias)?, expr(v)?)))
                    .collect::<Result<_, Status>>()?,
            )
        }
        "find_nearest" => find_nearest(args, proto)?,
        "sample" => {
            let mode = string("mode", &args[1])?;
            let size = match (&args[0].value_type, mode) {
                (_, "documents" | "percent") => &args[0],
                (_, other) => {
                    return Err(Status::invalid_argument(format!(
                        "Unsupported Sample Mode: {other}"
                    )));
                }
            };
            let size = match size.value_type {
                Some(ValueType::IntegerValue(i)) => i as f64,
                Some(ValueType::DoubleValue(d)) => d,
                _ => {
                    return Err(Status::invalid_argument(format!(
                        "Expected size to be INTEGER_VALUE or DOUBLE_VALUE, but was {}.",
                        value::proto_kind(size)
                    )));
                }
            };
            if size < 0.0 {
                return Err(Status::invalid_argument(
                    "The 'size' parameter must be non-negative.",
                ));
            }
            if mode == "percent" {
                if size > 1.0 {
                    return Err(Status::invalid_argument(
                        "The 'size' parameter must be between 0.0 and 1.0.",
                    ));
                }
                Stage::Sample(Sample::Percent(size))
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Stage::Sample(Sample::Documents(size as usize))
            }
        }
        "union" => {
            let Some(ValueType::PipelineValue(other)) = &args[0].value_type else {
                return Err(expected("other", "PIPELINE_VALUE", &args[0]));
            };
            if other.stages.is_empty() {
                return Err(Status::invalid_argument(
                    "The 'union(...)' stage must have at least one stage.",
                ));
            }
            Stage::Union(Box::new(pipeline(other)?))
        }
        "unnest" => {
            let source = expr(&args[0])?;
            let alias = field_path("alias", &args[1])?;
            let index = proto
                .options
                .get("index_field")
                .map(|v| field_path("index_field", v))
                .transpose()?;
            Stage::Unnest {
                expr: source,
                alias,
                index,
            }
        }
        "let" => Stage::Let(
            map_of("variables", &args[0])?
                .iter()
                .map(|(name, v)| Ok((name.clone(), expr(v)?)))
                .collect::<Result<_, Status>>()?,
        ),
        "update" => Stage::Update(aliases(name, "transforms", &args[0])?),
        "delete" => Stage::Delete,
        "search" => {
            if let Some(query) = proto.options.get("query") {
                expr(query)?;
            }
            return Err(Status::invalid_argument(
                "The search stage is not supported.",
            ));
        }
        _ => unreachable!("a stage spec() knows"),
    })
}

/// `Expected {argument} to be {kind}, but was {actual}.`
fn expected(argument: &str, kind: &str, actual: &Value) -> Status {
    Status::invalid_argument(format!(
        "Expected {argument} to be {kind}, but was {}.",
        value::proto_kind(actual)
    ))
}

fn reference<'a>(argument: &str, v: &'a Value) -> Result<&'a str, Status> {
    match &v.value_type {
        Some(ValueType::ReferenceValue(r)) => Ok(r),
        _ => Err(expected(argument, "REFERENCE_VALUE", v)),
    }
}

fn string<'a>(argument: &str, v: &'a Value) -> Result<&'a str, Status> {
    match &v.value_type {
        Some(ValueType::StringValue(s)) => Ok(s),
        _ => Err(expected(argument, "STRING_VALUE", v)),
    }
}

fn integer(argument: &str, v: &Value) -> Result<i64, Status> {
    match v.value_type {
        Some(ValueType::IntegerValue(i)) => Ok(i),
        _ => Err(expected(argument, "INTEGER_VALUE", v)),
    }
}

fn map_of<'a>(argument: &str, v: &'a Value) -> Result<&'a hidane_core::field_path::Fields, Status> {
    match &v.value_type {
        Some(ValueType::MapValue(map)) => Ok(&map.fields),
        _ => Err(expected(argument, "MAP_VALUE", v)),
    }
}

fn field_path(argument: &str, v: &Value) -> Result<FieldPath, Status> {
    match &v.value_type {
        Some(ValueType::FieldReferenceValue(path)) => {
            FieldPath::parse(path).map_err(Status::invalid_argument)
        }
        _ => Err(expected(argument, "FIELD_REFERENCE_VALUE", v)),
    }
}

/// A path of the stage's references, `/collection/document/…` relative to the database: a
/// collection's has an odd number of segments, a document's an even one.
fn path_of(what: &str, text: &str, document: bool) -> Result<ResourcePath, Status> {
    let lacks = |at: usize| {
        Status::invalid_argument(format!("{what} \"{text}\" lacks \"/\" at index {at}."))
    };
    let Some(rest) = text.strip_prefix('/') else {
        return Err(lacks(0));
    };
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(Status::invalid_argument(format!(
            "{what} \"{text}\" has an empty segment."
        )));
    }
    if (segments.len() % 2 == 0) != document {
        return Err(lacks(text.len()));
    }
    Ok(ResourcePath::from_segments(
        segments.iter().map(|s| (*s).to_owned()),
    ))
}

/// A map of field paths to expressions (`select`, `add_fields`, `update`).
fn aliases(stage: &str, argument: &str, v: &Value) -> Result<Vec<(FieldPath, Expr)>, Status> {
    map_of(argument, v)?
        .iter()
        .map(|(alias, v)| {
            if alias == "__name__" {
                return Err(Status::invalid_argument(format!(
                    "Stage '{stage}': field name '__name__' is reserved and can not be \
                     overwritten."
                )));
            }
            let path = FieldPath::parse(alias).map_err(Status::invalid_argument)?;
            Ok((path, expr(v)?))
        })
        .collect()
}

/// An alias that must be a top-level field (`aggregate`, `distinct`).
fn top_level(alias: &str) -> Result<String, Status> {
    let path = FieldPath::parse(alias).map_err(Status::invalid_argument)?;
    match path.segments() {
        [one] => Ok(one.clone()),
        _ => Err(Status::invalid_argument(format!(
            "The nested field reference '{alias}' is not supported in its current use case.\n\n\
             Certain stages like `aggregate(...)` or `unnest(...)` only support assigning to \
             top-level fields. This means you cannot use an alias like 'foo.bar' to assign the \
             result to a nested field. If you want to use an alias that has a dot in it, escape \
             the path with backticks, like `foo.bar`.\n\nDepending on the SDK, this can be done \
             like so:`.distinct(field(\"address.city\").as(\"`address.city`\"))`"
        ))),
    }
}

fn ordering(i: usize, v: &Value) -> Result<(Expr, bool), Status> {
    let map = map_of(&format!("ordering[{i}]"), v)?;
    let Some(expression) = map.get("expression") else {
        return Err(Status::invalid_argument(
            "Order is missing 'expression' field.",
        ));
    };
    let Some(direction) = map.get("direction") else {
        return Err(Status::invalid_argument(
            "Order is missing 'direction' field.",
        ));
    };
    let descending = match string("direction", direction)? {
        "ascending" => false,
        "descending" => true,
        other => {
            return Err(Status::invalid_argument(format!(
                "Unsupported direction '{other}'. Valid options are: [ascending, descending]."
            )));
        }
    };
    Ok((expr(expression)?, descending))
}

fn aggregate_stage(accumulators: &Value, groups: &Value) -> Result<Stage, Status> {
    let accumulators = map_of("accumulators", accumulators)?;
    let groups = map_of("groups", groups)?;
    if accumulators.is_empty() {
        return Err(Status::invalid_argument(
            "The 'aggregate(...)' stage requires at least one accumulator.",
        ));
    }
    let overlapping: Vec<&str> = accumulators
        .keys()
        .filter(|k| groups.contains_key(k))
        .map(String::as_str)
        .collect();
    if !overlapping.is_empty() {
        return Err(Status::invalid_argument(format!(
            "The 'aggregate(...)' stage cannot have overlapping field names: [{}]",
            overlapping.join(", ")
        )));
    }
    let accumulators = accumulators
        .iter()
        .map(|(alias, v)| Ok((top_level(alias)?, accumulator(v)?)))
        .collect::<Result<_, Status>>()?;
    let groups = groups
        .iter()
        .map(|(alias, v)| Ok((top_level(alias)?, expr(v)?)))
        .collect::<Result<_, Status>>()?;
    Ok(Stage::Aggregate {
        accumulators,
        groups,
    })
}

fn accumulator(v: &Value) -> Result<Accumulator, Status> {
    let Some(ValueType::FunctionValue(function)) = &v.value_type else {
        return Err(Status::invalid_argument(
            "Field references are only allowed in a scalar expression.",
        ));
    };
    let Function { name, args, .. } = function.as_ref();
    let def = aggregate::find(name)?;
    functions::check_arity(def.name, def.signatures, args.len())?;
    Ok(Accumulator {
        kind: def.kind,
        arg: args.first().map(expr).transpose()?,
    })
}

fn find_nearest(args: &[Value], proto: &ProtoStage) -> Result<Stage, Status> {
    let field = match &args[0].value_type {
        Some(ValueType::FieldReferenceValue(path)) => Expr::Field(Field::parse(path)?),
        _ => return Err(expected("field", "FIELD_REFERENCE_VALUE", &args[0])),
    };
    let vector = match &args[1].value_type {
        Some(ValueType::MapValue(map)) if is_vector(map) => {
            hidane_core::order::vector_elements(map)
                .iter()
                .map(|v| match v.value_type {
                    Some(ValueType::DoubleValue(d)) => d,
                    #[allow(clippy::cast_precision_loss)]
                    Some(ValueType::IntegerValue(i)) => i as f64,
                    _ => f64::NAN,
                })
                .collect()
        }
        Some(other) => {
            return Err(Status::invalid_argument(format!(
                "Expected vector to be VectorValue, but was {}.",
                vector_kind(other)
            )));
        }
        None => return Err(expected("vector", "VectorValue", &args[1])),
    };
    let measure = match string("distance_measure", &args[2])? {
        "euclidean" => Measure::Euclidean,
        "cosine" => Measure::Cosine,
        "dot_product" => Measure::DotProduct,
        other => {
            return Err(Status::invalid_argument(format!(
                "Unsupported distance_measure '{other}'. Valid options are: [euclidean, \
                 cosine, dot_product]."
            )));
        }
    };
    let limit = proto
        .options
        .get("limit")
        .map(|v| integer("limit", v))
        .transpose()?;
    let distance_field = proto
        .options
        .get("distance_field")
        .map(|v| field_path("distance_field", v))
        .transpose()?;
    Ok(Stage::FindNearest {
        field,
        vector,
        measure,
        limit,
        distance_field,
    })
}

/// A value's kind as `find_nearest` names it (`ArrayValue`).
fn vector_kind(v: &ValueType) -> String {
    let kind = value::proto_kind(&value::of(v.clone()));
    kind.trim_end_matches("_VALUE")
        .split('_')
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_string() + &chars.as_str().to_lowercase()
            })
        })
        .collect::<String>()
        + "Value"
}

/// Reads an expression.
pub(crate) fn expr(v: &Value) -> Result<Expr, Status> {
    Ok(match &v.value_type {
        Some(ValueType::FunctionValue(function)) => {
            let def = functions::scalar(&function.name)?;
            functions::check_arity(def.name, def.signatures, function.args.len())?;
            let args = function
                .args
                .iter()
                .map(expr)
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(check) = def.check {
                check(&args)?;
            }
            Expr::Call(Box::new(Call { def, args }))
        }
        Some(ValueType::FieldReferenceValue(path)) => Expr::Field(Field::parse(path)?),
        Some(ValueType::VariableReferenceValue(name)) => Expr::Variable(name.clone()),
        Some(ValueType::PipelineValue(p)) => Expr::Pipeline(Box::new(pipeline(p)?)),
        Some(ValueType::ArrayValue(array)) => {
            let items = array
                .values
                .iter()
                .map(expr)
                .collect::<Result<Vec<_>, _>>()?;
            if items.iter().all(|e| matches!(e, Expr::Constant(_))) {
                Expr::Constant(v.clone())
            } else {
                Expr::Array(items)
            }
        }
        Some(ValueType::MapValue(map)) if !is_vector(map) => {
            let entries = map
                .fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), expr(v)?)))
                .collect::<Result<Vec<_>, Status>>()?;
            if entries.iter().all(|(_, e)| matches!(e, Expr::Constant(_))) {
                Expr::Constant(v.clone())
            } else {
                Expr::Map(entries)
            }
        }
        _ => Expr::Constant(v.clone()),
    })
}
