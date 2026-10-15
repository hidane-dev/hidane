//! Running a pipeline's stages over rows. A source stage reads documents, in name order (the
//! official emulator returns them in an order of its own); every other stage makes rows of the
//! rows before it. The first error fails the whole pipeline.

use std::{cmp::Ordering, ops::ControlFlow, sync::Arc};

use hidane_core::{
    field_path::Fields,
    order::{compare, is_vector, vector_elements},
    path::ResourcePath,
    store::StoredDocument,
};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{
    Measure, Mode, Pipeline, Sample, Stage, aggregate,
    expr::{self, Env, Meta, Row, Scope},
    functions::compare::same,
    value,
};

/// Runs `pipeline`; its rows start with the variables `vars` (a subpipeline's: those visible
/// where it is evaluated), and its `subcollection` reads below `document` (the document of the
/// row a subpipeline is evaluated for; none, it reads nothing).
pub(crate) fn run(
    pipeline: &Pipeline,
    env: &Env<'_>,
    vars: &[(String, Value)],
    document: Option<&ResourcePath>,
) -> Result<Vec<Row>, Status> {
    let mut rows = Vec::new();
    for stage in &pipeline.stages {
        rows = apply(stage, rows, env, vars, document)?;
    }
    Ok(rows)
}

fn document_row(doc: &Arc<StoredDocument>, vars: &[(String, Value)]) -> Row {
    Row {
        doc: Some(Meta {
            path: Arc::clone(&doc.path),
            times: Some((doc.create_time, doc.update_time)),
        }),
        fields: doc.fields(),
        vars: vars.to_vec(),
    }
}

fn eval(row: &Row, env: &Env<'_>, e: &expr::Expr) -> expr::Eval {
    Scope::new(row, env).eval(e)
}

#[allow(clippy::too_many_lines)]
fn apply(
    stage: &Stage,
    rows: Vec<Row>,
    env: &Env<'_>,
    vars: &[(String, Value)],
    document: Option<&ResourcePath>,
) -> Result<Vec<Row>, Status> {
    let (store, database, at) = (env.store, env.database, env.at);
    let mut scanned = Vec::new();
    let mut collect = |doc: &Arc<StoredDocument>| {
        scanned.push(document_row(doc, vars));
        ControlFlow::Continue(())
    };
    Ok(match stage {
        Stage::Collection(path) => {
            store.scan_collection(database, path, at, &mut collect);
            scanned
        }
        Stage::Subcollection(id) => {
            if let Some(document) = document {
                store.scan_collection(database, &document.child(id.as_str()), at, &mut collect);
            }
            scanned
        }
        Stage::CollectionGroup(id) => {
            store.scan_collection_group(database, &ResourcePath::root(), id, at, &mut collect);
            scanned
        }
        Stage::Database => {
            store.scan_descendants(database, &ResourcePath::root(), at, &mut collect);
            scanned
        }
        Stage::Documents(paths) => paths
            .iter()
            .filter_map(|path| store.get(database, path, at))
            .map(|doc| document_row(&doc, vars))
            .collect(),
        Stage::Where(condition) => {
            let mut kept = Vec::with_capacity(rows.len());
            for row in rows {
                let decided = eval(&row, env, condition)?;
                if matches!(
                    decided.and_then(|v| v.value_type),
                    Some(ValueType::BooleanValue(true))
                ) {
                    kept.push(row);
                }
            }
            kept
        }
        Stage::Select(fields, keep_name) => rows
            .iter()
            .map(|row| {
                let mut out = Fields::new();
                for (path, e) in fields {
                    if let Some(v) = eval(row, env, e)? {
                        expr::set(&mut out, path, v);
                    }
                }
                Ok(Row {
                    doc: row.doc.as_ref().filter(|_| *keep_name).map(|doc| Meta {
                        path: Arc::clone(&doc.path),
                        times: None,
                    }),
                    fields: out,
                    vars: row.vars.clone(),
                })
            })
            .collect::<Result<_, Status>>()?,
        Stage::AddFields(fields) | Stage::Update(fields) => rows
            .into_iter()
            .map(|mut row| {
                let mut values = Vec::with_capacity(fields.len());
                for (path, e) in fields {
                    values.push((path, eval(&row, env, e)?));
                }
                for (path, v) in values {
                    if let Some(v) = v {
                        expr::set(&mut row.fields, path, v);
                    }
                }
                Ok(row)
            })
            .collect::<Result<_, Status>>()?,
        Stage::RemoveFields(paths) => rows
            .into_iter()
            .map(|mut row| {
                for path in paths {
                    expr::remove(&mut row.fields, path);
                }
                row
            })
            .collect(),
        Stage::ReplaceWith(e, mode) => rows
            .into_iter()
            .map(|row| {
                let map = match eval(&row, env, e)?.and_then(|v| v.value_type) {
                    Some(ValueType::MapValue(map)) if !is_vector(&map) => Some(map.fields),
                    _ => None,
                };
                Ok(match (mode, map) {
                    (Mode::FullReplace, map) => Row {
                        doc: None,
                        fields: map.unwrap_or_default(),
                        vars: row.vars,
                    },
                    (_, None) => row,
                    (Mode::MergeOverwrite, Some(map)) => {
                        let mut row = row;
                        for (k, v) in map.iter() {
                            row.fields.insert(k.clone(), v.clone());
                        }
                        row
                    }
                    (Mode::MergeKeep, Some(map)) => {
                        let mut row = row;
                        for (k, v) in map.iter() {
                            if !row.fields.contains_key(k) {
                                row.fields.insert(k.clone(), v.clone());
                            }
                        }
                        row
                    }
                })
            })
            .collect::<Result<_, Status>>()?,
        Stage::Sort(orderings) => {
            let mut keyed = Vec::with_capacity(rows.len());
            for row in rows {
                let mut keys = Vec::with_capacity(orderings.len());
                for (e, _) in orderings {
                    keys.push(eval(&row, env, e)?);
                }
                keyed.push((keys, row));
            }
            keyed.sort_by(|(a, _), (b, _)| {
                for ((x, y), (_, descending)) in a.iter().zip(b).zip(orderings) {
                    let order = order_values(x.as_ref(), y.as_ref());
                    let order = if *descending { order.reverse() } else { order };
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                Ordering::Equal
            });
            keyed.into_iter().map(|(_, row)| row).collect()
        }
        Stage::Limit(n) => rows
            .into_iter()
            .take(usize::try_from(*n).unwrap_or(usize::MAX))
            .collect(),
        Stage::Offset(n) => rows
            .into_iter()
            .skip(usize::try_from(*n).unwrap_or(usize::MAX))
            .collect(),
        Stage::Aggregate {
            accumulators,
            groups,
        } => aggregate_rows(&rows, accumulators, groups, env, vars)?,
        Stage::Distinct(fields) => {
            let mut seen: Vec<Vec<Value>> = Vec::new();
            let mut out = Vec::new();
            for row in &rows {
                let mut key = Vec::with_capacity(fields.len());
                for (_, e) in fields {
                    key.push(eval(row, env, e)?.unwrap_or_else(value::null));
                }
                if seen.iter().any(|k| same_key(k, &key)) {
                    continue;
                }
                let mut fields_out = Fields::new();
                for ((alias, _), v) in fields.iter().zip(&key) {
                    fields_out.insert(alias.clone(), v.clone());
                }
                seen.push(key);
                out.push(Row {
                    doc: None,
                    fields: fields_out,
                    vars: row.vars.clone(),
                });
            }
            out
        }
        Stage::FindNearest {
            field,
            vector,
            measure,
            limit,
            distance_field,
        } => {
            let mut scored = Vec::new();
            for row in rows {
                let Some(v) = eval(&row, env, field)? else {
                    continue;
                };
                let Some(ValueType::MapValue(map)) = &v.value_type else {
                    continue;
                };
                if !is_vector(map) {
                    continue;
                }
                let elements: Vec<f64> = vector_elements(map).iter().map(as_f64).collect();
                if elements.len() != vector.len() {
                    continue;
                }
                let Some(distance) = distance(*measure, &elements, vector) else {
                    continue;
                };
                scored.push((distance, row));
            }
            scored.sort_by(|(a, _), (b, _)| {
                let order = a.partial_cmp(b).unwrap_or(Ordering::Equal);
                if *measure == Measure::DotProduct {
                    order.reverse()
                } else {
                    order
                }
            });
            if let Some(limit) = limit {
                scored.truncate(usize::try_from(*limit).unwrap_or(usize::MAX));
            }
            scored
                .into_iter()
                .map(|(distance, mut row)| {
                    if let Some(path) = distance_field {
                        expr::set(&mut row.fields, path, value::double(distance));
                    }
                    row
                })
                .collect()
        }
        Stage::Sample(Sample::Documents(n)) => {
            if *n >= rows.len() {
                rows
            } else {
                let mut indexed: Vec<(u64, Row)> =
                    rows.into_iter().map(|row| (random_u64(), row)).collect();
                indexed.sort_by_key(|(k, _)| *k);
                indexed.into_iter().take(*n).map(|(_, row)| row).collect()
            }
        }
        Stage::Sample(Sample::Percent(p)) => rows
            .into_iter()
            .filter(|_| {
                #[allow(clippy::cast_precision_loss)]
                let unit = (random_u64() >> 11) as f64 / (1_u64 << 53) as f64;
                unit < *p
            })
            .collect(),
        Stage::Union(other) => {
            let mut rows = rows;
            rows.extend(run(other, env, vars, document)?);
            rows
        }
        Stage::Unnest {
            expr: e,
            alias,
            index,
        } => {
            let mut out = Vec::new();
            for row in rows {
                match eval(&row, env, e)? {
                    None => out.push(row),
                    Some(Value {
                        value_type: Some(ValueType::ArrayValue(array)),
                    }) => {
                        for (i, element) in array.values.into_iter().enumerate() {
                            let mut each = row.clone();
                            expr::set(&mut each.fields, alias, element);
                            if let Some(index) = index {
                                expr::set(
                                    &mut each.fields,
                                    index,
                                    value::int(i64::try_from(i).unwrap_or(i64::MAX)),
                                );
                            }
                            out.push(each);
                        }
                    }
                    Some(other) => {
                        let mut row = row;
                        expr::set(&mut row.fields, alias, other);
                        out.push(row);
                    }
                }
            }
            out
        }
        Stage::Let(definitions) => rows
            .into_iter()
            .map(|mut row| {
                for (name, e) in definitions {
                    let v = eval(&row, env, e)?.unwrap_or_else(value::null);
                    row.vars.push((name.clone(), v));
                }
                Ok(row)
            })
            .collect::<Result<_, Status>>()?,
        Stage::Delete => rows,
    })
}

/// Firestore's order, an absent value with null.
pub(crate) fn order_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    let null = value::null();
    compare(a.unwrap_or(&null), b.unwrap_or(&null))
}

fn same_key(a: &[Value], b: &[Value]) -> bool {
    a.iter().zip(b).all(|(x, y)| same(x, y))
}

fn aggregate_rows(
    rows: &[Row],
    accumulators: &[(String, aggregate::Accumulator)],
    groups: &[(String, expr::Expr)],
    env: &Env<'_>,
    vars: &[(String, Value)],
) -> Result<Vec<Row>, Status> {
    // Each group's key and the arguments of every accumulator in each of its rows.
    let mut keys: Vec<Vec<Value>> = Vec::new();
    let mut inputs: Vec<Vec<Vec<Option<Value>>>> = Vec::new();
    for row in rows {
        let mut key = Vec::with_capacity(groups.len());
        for (_, e) in groups {
            key.push(eval(row, env, e)?.unwrap_or_else(value::null));
        }
        let mut args = Vec::with_capacity(accumulators.len());
        for (_, accumulator) in accumulators {
            args.push(match &accumulator.arg {
                Some(e) => eval(row, env, e)?,
                None => None,
            });
        }
        let at = match keys.iter().position(|k| same_key(k, &key)) {
            Some(at) => at,
            None => {
                keys.push(key);
                inputs.push(vec![Vec::new(); accumulators.len()]);
                keys.len() - 1
            }
        };
        for (i, arg) in args.into_iter().enumerate() {
            inputs[at][i].push(arg);
        }
    }
    if keys.is_empty() && groups.is_empty() {
        keys.push(Vec::new());
        inputs.push(vec![Vec::new(); accumulators.len()]);
    }
    keys.into_iter()
        .zip(inputs)
        .map(|(key, values)| {
            let mut fields = Fields::new();
            for ((alias, _), v) in groups.iter().zip(key) {
                fields.insert(alias.clone(), v);
            }
            for ((alias, accumulator), values) in accumulators.iter().zip(values) {
                let result =
                    aggregate::compute(accumulator.kind, &values, accumulator.arg.is_some())?;
                fields.insert(alias.clone(), result);
            }
            Ok(Row {
                doc: None,
                fields,
                vars: vars.to_vec(),
            })
        })
        .collect()
}

fn as_f64(v: &Value) -> f64 {
    match v.value_type {
        Some(ValueType::DoubleValue(d)) => d,
        #[allow(clippy::cast_precision_loss)]
        Some(ValueType::IntegerValue(i)) => i as f64,
        _ => f64::NAN,
    }
}

/// The distance between two vectors of one dimension; `None` where it is undefined (a zero
/// vector's cosine).
pub(crate) fn distance(measure: Measure, a: &[f64], b: &[f64]) -> Option<f64> {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    match measure {
        Measure::DotProduct => Some(dot),
        Measure::Euclidean => Some(
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y) * (x - y))
                .sum::<f64>()
                .sqrt(),
        ),
        Measure::Cosine => {
            // One square root of the product, as `cosine_distance` computes it: two would
            // make vectors of one direction differ in the last bit.
            let squares = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>();
            let product = squares(a) * squares(b);
            if product == 0.0 {
                return None;
            }
            Some(1.0 - dot / product.sqrt())
        }
    }
}

fn random_u64() -> u64 {
    use std::{
        hash::{BuildHasher, Hasher},
        sync::atomic::{AtomicU64, Ordering},
    };
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}
