//! Maps: `map_get`, `map_set`, `map_remove`, `map_merge`, `map_keys`, `map_values`,
//! `map_entries`, `get_field`.
//!
//! A vector is not a map here. Keys name one entry each, never a path (`get_field(m, 'a.b')`
//! looks for the key `a.b`), and results keep the map's order, new keys last.
//!
//! `map_set` and `get_field` differ from the rest: `map_set` takes its keys as string literals
//! and gives absent, not an error, for anything but a map, as `get_field` does too; an absent
//! value removes its key. `get_field`'s errors name it `field(...)`, and its key may be neither
//! null nor absent (`UNSET`).

use hidane_core::{field_path::Fields, order::is_vector};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{Def, Signature, args, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Expr, Scope},
    value,
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "map_get",
        signatures: &[Signature::fixed(
            "map_get(map: map[string, any], key: string): any?",
            2,
        )],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            let map = map("map_get", values[0].as_ref())?;
            let key = string("map_get", values[1].as_ref())?;
            match (map, key) {
                (Some(map), Some(key)) => Ok(map.get(key).cloned()),
                _ => super::null(),
            }
        },
    },
    Def {
        name: "map_set",
        signatures: &[Signature::variadic(
            "map_set(map: map[string, any], entries: any*): map[string, any]",
            3,
        )],
        check: Some(check_map_set),
        eval: |call, scope| {
            let mut values = args(call, scope)?.into_iter();
            let Some(Some(Value {
                value_type: Some(ValueType::MapValue(map)),
            })) = values.next()
            else {
                return Ok(None);
            };
            if is_vector(&map) {
                return Ok(None);
            }
            let mut fields = map.fields;
            let keys = call.args[1..].iter().step_by(2);
            for (key, v) in keys.zip(values.skip(1).step_by(2)) {
                let key = key.literal_string().unwrap_or_default();
                match v {
                    Some(v) => fields.insert(key.to_owned(), v),
                    None => fields.remove(key),
                };
            }
            Ok(Some(value::map(fields)))
        },
    },
    Def {
        name: "map_remove",
        signatures: &[Signature::variadic(
            "map_remove(map: map[string, any], key: string*): map[string, any]",
            2,
        )],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            let map = map("map_remove", values[0].as_ref())?;
            let mut keys = Vec::with_capacity(values.len() - 1);
            for v in &values[1..] {
                keys.push(string("map_remove", v.as_ref())?);
            }
            let (Some(map), Some(keys)) = (map, keys.into_iter().collect::<Option<Vec<_>>>())
            else {
                return super::null();
            };
            let mut fields = map.clone();
            for key in keys {
                fields.remove(key);
            }
            Ok(Some(value::map(fields)))
        },
    },
    Def {
        name: "map_merge",
        signatures: &[Signature::variadic(
            "map_merge(maps: map[string, any]*): map[string, any]",
            2,
        )],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            let mut maps = Vec::with_capacity(values.len());
            for v in &values {
                maps.push(map("map_merge", v.as_ref())?);
            }
            let Some(maps) = maps.into_iter().collect::<Option<Vec<_>>>() else {
                return super::null();
            };
            let mut fields = Fields::new();
            for map in maps {
                fields.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            Ok(Some(value::map(fields)))
        },
    },
    Def {
        name: "map_keys",
        signatures: &[Signature::fixed(
            "map_keys(map: map[string, any]): array[string]",
            1,
        )],
        check: None,
        eval: |call, scope| listing(call, scope, "map_keys", |k, _| value::string(k.as_str())),
    },
    Def {
        name: "map_values",
        signatures: &[Signature::fixed(
            "map_values(map: map[string, any]): array[any]",
            1,
        )],
        check: None,
        eval: |call, scope| listing(call, scope, "map_values", |_, v| v.clone()),
    },
    Def {
        name: "map_entries",
        signatures: &[Signature::fixed(
            "map_entries(map: map[string, any]): array[map[string, any]]",
            1,
        )],
        check: None,
        eval: |call, scope| {
            listing(call, scope, "map_entries", |k, v| {
                value::map(Fields::from([
                    ("k".to_owned(), value::string(k.as_str())),
                    ("v".to_owned(), v.clone()),
                ]))
            })
        },
    },
    Def {
        name: "get_field",
        signatures: &[Signature::fixed(
            "get_field(map: map[string, any], key: string): any?",
            2,
        )],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            let key = match &values[1] {
                Some(Value {
                    value_type: Some(ValueType::StringValue(key)),
                }) => key,
                Some(other) => return Err(requires("field", "String", other)),
                None => {
                    return Err(Status::invalid_argument(
                        "The function field(...) requires `String` but got `UNSET`.",
                    ));
                }
            };
            Ok(match &values[0] {
                Some(Value {
                    value_type: Some(ValueType::MapValue(map)),
                }) if !is_vector(map) => map.fields.get(key).cloned(),
                _ => None,
            })
        },
    },
];

/// `map_set(map, key, value, ...)`: an odd number of arguments, each key a string literal.
fn check_map_set(args: &[Expr]) -> Result<(), Status> {
    if args.len().is_multiple_of(2) {
        return Err(Status::invalid_argument(
            "'map_set' function must have an odd number of arguments",
        ));
    }
    if args[1..]
        .iter()
        .step_by(2)
        .any(|key| key.literal_string().is_none())
    {
        return Err(Status::invalid_argument(
            "map_set keys must be constants/literals",
        ));
    }
    Ok(())
}

/// A map operand: `None` for null or absent, an error for any other type (a vector too).
fn map<'a>(function: &str, v: Option<&'a Value>) -> Result<Option<&'a Fields>, Status> {
    match v {
        None => Ok(None),
        Some(v) => match &v.value_type {
            Some(ValueType::MapValue(map)) if !is_vector(map) => Ok(Some(&map.fields)),
            _ if value::is_null(v) => Ok(None),
            _ => Err(requires(function, "Map", v)),
        },
    }
}

/// A key operand: `None` for null or absent, an error for any other type than a string.
fn string<'a>(function: &str, v: Option<&'a Value>) -> Result<Option<&'a str>, Status> {
    match v {
        None => Ok(None),
        Some(v) => match &v.value_type {
            Some(ValueType::StringValue(s)) => Ok(Some(s)),
            _ if value::is_null(v) => Ok(None),
            _ => Err(requires(function, "String", v)),
        },
    }
}

/// An array of one value per entry of the map argument, in the map's order.
fn listing(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
    entry: fn(&String, &Value) -> Value,
) -> Eval {
    let values = args(call, scope)?;
    Ok(Some(match map(function, values[0].as_ref())? {
        Some(map) => value::array(map.iter().map(|(k, v)| entry(k, v)).collect()),
        None => value::null(),
    }))
}
