//! Strings and bytes: `char_length`, `byte_length`, `to_lower`, `to_upper`, `trim`, `ltrim`,
//! `rtrim`, `reverse`, `string_reverse`, `substring`, `string_concat`, `concat`, `starts_with`,
//! `ends_with`, `string_contains`, `like`, `regex_contains`, `regex_match`, `regex_find`,
//! `regex_find_all`, `split`, `join`, `string_index_of`, `string_repeat`, `string_replace_all`,
//! `string_replace_one`, `length`.
//!
//! Strings count, index and reverse in code points, bytes in bytes. A function of several
//! strings or bytes takes them all of one type: the error for a mix names the last argument.
//! Every argument is checked for its type before a null or absent one makes the result null.
//! `substring` positions count from 0, from the end when negative; an empty `split` delimiter
//! splits into code points (bytes); `like` patterns match `%` to any characters and `_` to one.
//! What the recordings do not show follows GoogleSQL: `split` without a delimiter splits at
//! commas, `join` leaves out null elements unless given a text for them, and `\` escapes a
//! `like` character. Regular expressions are RE2's, run by the `regex` crate with RE2's
//! ASCII-only `\d`, `\w`, `\s` and `\b`, and refused with RE2's messages.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use hidane_core::order::{is_vector, vector_elements};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use regex::Regex;
use tonic::Status;

use super::{Def, Signature, args, math::integer, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Scope},
    value,
};

/// What the errors of most functions expect of the first argument and of the others.
const EITHER: (&str, &str) = ("String | Bytes", "String | Bytes");

/// What the errors of the functions of a search space and a pattern expect.
const PATTERN: &[&str] = &["String", "String or Regex"];

/// The longest string or bytes `string_repeat` makes.
const MAX_REPEAT: usize = 64 << 20;

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "char_length",
        signatures: &[Signature::fixed("char_length(str: string): int64", 1)],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "char_length", &["String"], |s| {
                Ok(count(s[0].chars().count()))
            })
        },
    },
    Def {
        name: "byte_length",
        signatures: &[Signature::fixed(
            "byte_length[T: string | bytes](input: T): int64",
            1,
        )],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "byte_length", EITHER, |t| {
                Ok(count(match t {
                    Texts::Strings(s) => s[0].len(),
                    Texts::Bytes(b) => b[0].len(),
                }))
            })
        },
    },
    Def {
        name: "to_lower",
        signatures: &[Signature::fixed(
            "to_lower[T: string | bytes](input: T): T",
            1,
        )],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "to_lower", EITHER, |t| {
                Ok(match t {
                    Texts::Strings(s) => value::string(s[0].to_lowercase()),
                    Texts::Bytes(b) => value::bytes(b[0].to_ascii_lowercase()),
                })
            })
        },
    },
    Def {
        name: "to_upper",
        signatures: &[Signature::fixed(
            "to_upper[T: string | bytes](input: T): T",
            1,
        )],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "to_upper", EITHER, |t| {
                Ok(match t {
                    Texts::Strings(s) => value::string(s[0].to_uppercase()),
                    Texts::Bytes(b) => value::bytes(b[0].to_ascii_uppercase()),
                })
            })
        },
    },
    Def {
        name: "trim",
        signatures: &[
            Signature::fixed("trim[T: string | bytes](input: T): T", 1),
            Signature::fixed("trim[T: string | bytes](input: T, values_to_trim: T): T", 2),
        ],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "trim", ("String | Bytes", "String"), |t| {
                Ok(trim(&t, true, true))
            })
        },
    },
    Def {
        name: "ltrim",
        signatures: &[
            Signature::fixed("ltrim[T: string | bytes](value: T): T", 1),
            Signature::fixed("ltrim[T: string | bytes](value: T, to_trim: T): T", 2),
        ],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "ltrim", ("String | Bytes", "String"), |t| {
                Ok(trim(&t, true, false))
            })
        },
    },
    Def {
        name: "rtrim",
        signatures: &[
            Signature::fixed("rtrim[T: string | bytes](value: T): T", 1),
            Signature::fixed("rtrim[T: string | bytes](value: T, to_trim: T): T", 2),
        ],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "rtrim", ("String | Bytes", "String"), |t| {
                Ok(trim(&t, false, true))
            })
        },
    },
    Def {
        name: "reverse",
        signatures: &[Signature::fixed(
            "reverse[T: string | bytes | array[any]](input: T): T",
            1,
        )],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            if let Some(Value {
                value_type: Some(ValueType::ArrayValue(array)),
            }) = &values[0]
            {
                return Ok(Some(value::array(
                    array.values.iter().rev().cloned().collect(),
                )));
            }
            let expected = "String | Bytes | Array";
            with_texts("reverse", (expected, expected), &values, |t| {
                Ok(reversed(&t))
            })
        },
    },
    Def {
        name: "string_reverse",
        signatures: &[Signature::fixed(
            "string_reverse[T: string | bytes](input: T): T",
            1,
        )],
        check: None,
        eval: |call, scope| on_texts(call, scope, "string_reverse", EITHER, |t| Ok(reversed(&t))),
    },
    Def {
        name: "substring",
        signatures: &[
            Signature::fixed(
                "substring[T: string | bytes](input: T, position: int64): T",
                2,
            ),
            Signature::fixed(
                "substring[T: string | bytes](input: T, position: int64, length: int64): T",
                3,
            ),
        ],
        check: None,
        eval: substring,
    },
    Def {
        name: "string_concat",
        signatures: &[Signature::variadic(
            "string_concat(strings: string*): string",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "string_concat", &["String"], |s| {
                Ok(value::string(s.concat()))
            })
        },
    },
    Def {
        name: "concat",
        signatures: &[Signature::variadic(
            "concat[T: string | bytes | array[any]](input: T*): T",
            2,
        )],
        check: None,
        eval: concat,
    },
    Def {
        name: "starts_with",
        signatures: &[Signature::fixed(
            "starts_with(value: string, prefix: string): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "starts_with", &["String"], |s| {
                Ok(value::boolean(s[0].starts_with(s[1])))
            })
        },
    },
    Def {
        name: "ends_with",
        signatures: &[Signature::fixed(
            "ends_with(value: string, postfix: string): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "ends_with", &["String"], |s| {
                Ok(value::boolean(s[0].ends_with(s[1])))
            })
        },
    },
    Def {
        name: "string_contains",
        signatures: &[Signature::fixed(
            "string_contains(value: string, substring: string): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "string_contains", &["String"], |s| {
                Ok(value::boolean(s[0].contains(s[1])))
            })
        },
    },
    Def {
        name: "like",
        signatures: &[Signature::fixed(
            "like[T: string | regex](search_space: string, pattern: T): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "like", PATTERN, |s| {
                Ok(value::boolean(like(s[1])?.is_match(s[0])))
            })
        },
    },
    Def {
        name: "regex_contains",
        signatures: &[Signature::fixed(
            "regex_contains[T: string | regex](search_space: string, regexp: T): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "regex_contains", PATTERN, |s| {
                Ok(value::boolean(regex(s[1], false)?.is_match(s[0])))
            })
        },
    },
    Def {
        name: "regex_match",
        signatures: &[Signature::fixed(
            "regex_match[T: string | regex](search_space: string, regexp: T): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "regex_match", PATTERN, |s| {
                Ok(value::boolean(regex(s[1], true)?.is_match(s[0])))
            })
        },
    },
    Def {
        name: "regex_find",
        signatures: &[Signature::fixed(
            "regex_find[T: string | regex](search_space: string, regexp: T): string?",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "regex_find", PATTERN, |s| {
                let re = extraction(s[1])?;
                Ok(re
                    .captures(s[0])
                    .map_or_else(value::null, |found| extracted(&re, &found)))
            })
        },
    },
    Def {
        name: "regex_find_all",
        signatures: &[Signature::fixed(
            "regex_find_all[T: string | regex](search_space: string, regexp: T): array[string?]",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_strings(call, scope, "regex_find_all", PATTERN, |s| {
                let re = extraction(s[1])?;
                Ok(value::array(
                    re.captures_iter(s[0])
                        .map(|found| extracted(&re, &found))
                        .collect(),
                ))
            })
        },
    },
    Def {
        name: "split",
        signatures: &[
            Signature::fixed("split(input: string): array[string]", 1),
            Signature::fixed(
                "split[T: string | bytes](input: T, delimiter: T): array[T]",
                2,
            ),
        ],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            if values.len() == 1 {
                return with_strings("split", &["String"], &values, |s| {
                    Ok(split_string(s[0], ","))
                });
            }
            with_texts("split", ("String", "String"), &values, |t| {
                Ok(match t {
                    Texts::Strings(s) => split_string(s[0], s[1]),
                    Texts::Bytes(b) => split_bytes(b[0], b[1]),
                })
            })
        },
    },
    Def {
        name: "join",
        signatures: &[
            Signature::fixed(
                "join[T: string | bytes](array: array[T], delimiter: T): string",
                2,
            ),
            Signature::fixed(
                "join[T: string | bytes](array: array[T], delimiter: T, null_text: T): string",
                3,
            ),
        ],
        check: None,
        eval: join,
    },
    Def {
        name: "string_index_of",
        signatures: &[Signature::fixed(
            "string_index_of[T: string | bytes](value: T, search: T): int64",
            2,
        )],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "string_index_of", ("String", "String"), |t| {
                let index = match t {
                    Texts::Strings(s) => s[0].find(s[1]).map(|at| s[0][..at].chars().count()),
                    Texts::Bytes(b) => find_bytes(b[0], b[1]),
                };
                Ok(index.map_or_else(|| value::int(-1), count))
            })
        },
    },
    Def {
        name: "string_repeat",
        signatures: &[Signature::fixed(
            "string_repeat[T: string | bytes](input: T, repetitions: int64): T",
            2,
        )],
        check: None,
        eval: repeat,
    },
    Def {
        name: "string_replace_all",
        signatures: &[Signature::fixed(
            "string_replace_all[T: string | bytes](input: T, find: T, replacement: T): T",
            3,
        )],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "string_replace_all", EITHER, |t| {
                Ok(replace(&t, false))
            })
        },
    },
    Def {
        name: "string_replace_one",
        signatures: &[Signature::fixed(
            "string_replace_one[T: string | bytes](input: T, find: T, replacement: T): T",
            3,
        )],
        check: None,
        eval: |call, scope| {
            on_texts(call, scope, "string_replace_one", EITHER, |t| {
                Ok(replace(&t, true))
            })
        },
    },
    Def {
        name: "length",
        signatures: &[Signature::fixed(
            "length[T: string | bytes | array[any] | map[string, any] | vector](input: T): T",
            1,
        )],
        check: None,
        eval: |call, scope| {
            let Some(v) = scope.eval(&call.args[0])? else {
                return super::null();
            };
            Ok(Some(match &v.value_type {
                None | Some(ValueType::NullValue(_)) => value::null(),
                Some(ValueType::StringValue(s)) => count(s.chars().count()),
                Some(ValueType::BytesValue(b)) => count(b.len()),
                Some(ValueType::ArrayValue(array)) => count(array.values.len()),
                Some(ValueType::MapValue(map)) if is_vector(map) => {
                    count(vector_elements(map).len())
                }
                Some(ValueType::MapValue(map)) => count(map.fields.len()),
                Some(_) => {
                    return Err(requires(
                        "length",
                        "String | Bytes | Array | Vector | Map",
                        &v,
                    ));
                }
            }))
        },
    },
];

/// String or bytes arguments, all of one type.
enum Texts<'a> {
    Strings(Vec<&'a str>),
    Bytes(Vec<&'a [u8]>),
}

/// `values` as [`Texts`], `None` if one is null or absent. An argument of another type is an
/// error naming `first` for the first argument and `rest` for the others; a mix of strings and
/// bytes is one naming `rest` and the last argument.
fn texts<'a>(
    function: &str,
    (first, rest): (&str, &str),
    values: &'a [Option<Value>],
) -> Result<Option<Texts<'a>>, Status> {
    let mut strings = Vec::with_capacity(values.len());
    let mut bytes = Vec::new();
    let mut null = false;
    for (i, v) in values.iter().enumerate() {
        match v.as_ref().and_then(|v| v.value_type.as_ref()) {
            None | Some(ValueType::NullValue(_)) => null = true,
            Some(ValueType::StringValue(s)) => strings.push(s.as_str()),
            Some(ValueType::BytesValue(b)) => bytes.push(b.as_slice()),
            Some(_) => {
                let expected = if i == 0 { first } else { rest };
                return Err(requires(function, expected, v.as_ref().expect("a value")));
            }
        }
    }
    if null {
        return Ok(None);
    }
    match (strings.is_empty(), bytes.is_empty()) {
        (_, true) => Ok(Some(Texts::Strings(strings))),
        (true, false) => Ok(Some(Texts::Bytes(bytes))),
        (false, false) => {
            let last = values.last().and_then(Option::as_ref).expect("a value");
            Err(requires(function, rest, last))
        }
    }
}

/// `f` of `values` as [`Texts`]: null when one is null or absent.
fn with_texts(
    function: &str,
    expected: (&str, &str),
    values: &[Option<Value>],
    f: impl FnOnce(Texts<'_>) -> Result<Value, Status>,
) -> Eval {
    match texts(function, expected, values)? {
        Some(texts) => f(texts).map(Some),
        None => super::null(),
    }
}

/// `f` of the call's arguments as [`Texts`].
fn on_texts(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
    expected: (&str, &str),
    f: impl FnOnce(Texts<'_>) -> Result<Value, Status>,
) -> Eval {
    with_texts(function, expected, &args(call, scope)?, f)
}

/// `f` of string arguments: null when one is null or absent, an error naming `expected[i]`
/// (or the last of them) for argument `i` of another type.
fn with_strings(
    function: &str,
    expected: &[&str],
    values: &[Option<Value>],
    f: impl FnOnce(&[&str]) -> Result<Value, Status>,
) -> Eval {
    let mut strings = Vec::with_capacity(values.len());
    let mut null = false;
    for (i, v) in values.iter().enumerate() {
        match v.as_ref().and_then(|v| v.value_type.as_ref()) {
            None | Some(ValueType::NullValue(_)) => null = true,
            Some(ValueType::StringValue(s)) => strings.push(s.as_str()),
            Some(_) => {
                let expected = expected[i.min(expected.len() - 1)];
                return Err(requires(function, expected, v.as_ref().expect("a value")));
            }
        }
    }
    if null {
        return super::null();
    }
    f(&strings).map(Some)
}

/// `f` of the call's arguments as strings.
fn on_strings(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
    expected: &[&str],
    f: impl FnOnce(&[&str]) -> Result<Value, Status>,
) -> Eval {
    with_strings(function, expected, &args(call, scope)?, f)
}

fn count(n: usize) -> Value {
    value::int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn invalid(message: &str) -> Status {
    Status::invalid_argument(message)
}

fn reversed(texts: &Texts<'_>) -> Value {
    match texts {
        Texts::Strings(s) => value::string(s[0].chars().rev().collect::<String>()),
        Texts::Bytes(b) => value::bytes(b[0].iter().rev().copied().collect()),
    }
}

/// `trim` (`start` and `end`), `ltrim` or `rtrim`: whitespace, or the characters (bytes) of the
/// second argument.
fn trim(texts: &Texts<'_>, start: bool, end: bool) -> Value {
    match texts {
        Texts::Strings(s) => {
            let set = s.get(1).copied();
            let strip = |c: char| set.map_or(c.is_whitespace(), |set| set.contains(c));
            let mut t = s[0];
            if start {
                t = t.trim_start_matches(strip);
            }
            if end {
                t = t.trim_end_matches(strip);
            }
            value::string(t)
        }
        Texts::Bytes(b) => {
            let set = b.get(1).copied();
            let keep = |x: &u8| !set.map_or(x.is_ascii_whitespace(), |set| set.contains(x));
            let mut t = b[0];
            if start {
                t = &t[t.iter().position(keep).unwrap_or(t.len())..];
            }
            if end {
                t = &t[..t.iter().rposition(keep).map_or(0, |i| i + 1)];
            }
            value::bytes(t.to_vec())
        }
    }
}

/// `substring(input, position[, length])`: from `position`, counted from the end when negative
/// and clamped to the input, `length` code points (bytes) or to the end.
fn substring(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let input = texts("substring", EITHER, &values[..1])?;
    let position = integer("substring", values[1].as_ref())?;
    let length = values
        .get(2)
        .map(|v| integer("substring", v.as_ref()))
        .transpose()?;
    let (Some(input), Some(position)) = (input, position) else {
        return super::null();
    };
    let length = match length {
        None => None,
        Some(None) => return super::null(),
        Some(Some(length)) if length < 0 => {
            return Err(invalid("substring(...) length must be non-negative"));
        }
        Some(Some(length)) => Some(length),
    };
    let range = |len: usize| {
        let len = i64::try_from(len).unwrap_or(i64::MAX);
        let from = if position < 0 {
            (len + position).max(0)
        } else {
            position.min(len)
        };
        let to = length.map_or(len, |length| from.saturating_add(length).min(len));
        let index = |i: i64| usize::try_from(i).unwrap_or(0);
        index(from)..index(to)
    };
    Ok(Some(match input {
        Texts::Strings(s) => {
            let chars: Vec<char> = s[0].chars().collect();
            value::string(chars[range(chars.len())].iter().collect::<String>())
        }
        Texts::Bytes(b) => value::bytes(b[0][range(b[0].len())].to_vec()),
    }))
}

/// `concat` of strings, bytes or arrays: the first argument that is not null decides which,
/// and the errors for the others name its type.
fn concat(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let mut first: Option<&Value> = None;
    let mut null = false;
    for v in &values {
        let Some(v) = v.as_ref().filter(|v| !value::is_null(v)) else {
            null = true;
            continue;
        };
        match first {
            Some(first) if value::kind(v) != value::kind(first) => {
                return Err(requires("concat", value::kind(first), v));
            }
            Some(_) => {}
            None if matches!(
                v.value_type,
                Some(
                    ValueType::StringValue(_) | ValueType::BytesValue(_) | ValueType::ArrayValue(_)
                )
            ) =>
            {
                first = Some(v);
            }
            None => return Err(requires("concat", "String | Bytes | Array", v)),
        }
    }
    if null {
        return super::null();
    }
    let mut string = String::new();
    let mut bytes = Vec::new();
    let mut array = Vec::new();
    for v in values.iter().flatten() {
        match &v.value_type {
            Some(ValueType::StringValue(s)) => string.push_str(s),
            Some(ValueType::BytesValue(b)) => bytes.extend_from_slice(b),
            Some(ValueType::ArrayValue(a)) => array.extend_from_slice(&a.values),
            _ => {}
        }
    }
    Ok(Some(match first.and_then(|v| v.value_type.as_ref()) {
        Some(ValueType::BytesValue(_)) => value::bytes(bytes),
        Some(ValueType::ArrayValue(_)) => value::array(array),
        _ => value::string(string),
    }))
}

/// `split` of a string: at each `delimiter`, or into its code points when that is empty.
fn split_string(input: &str, delimiter: &str) -> Value {
    if input.is_empty() {
        return value::array(vec![value::string("")]);
    }
    value::array(if delimiter.is_empty() {
        input
            .chars()
            .map(|c| value::string(c.to_string()))
            .collect()
    } else {
        input.split(delimiter).map(value::string).collect()
    })
}

/// `split` of bytes: at each `delimiter`, or into single bytes when that is empty.
fn split_bytes(input: &[u8], delimiter: &[u8]) -> Value {
    if input.is_empty() || delimiter.is_empty() {
        let parts: Vec<&[u8]> = if input.is_empty() {
            vec![input]
        } else {
            input.chunks(1).collect()
        };
        return value::array(
            parts
                .into_iter()
                .map(|p| value::bytes(p.to_vec()))
                .collect(),
        );
    }
    let mut parts = Vec::new();
    let mut rest = input;
    while let Some(at) = find_bytes(rest, delimiter) {
        parts.push(value::bytes(rest[..at].to_vec()));
        rest = &rest[at + delimiter.len()..];
    }
    parts.push(value::bytes(rest.to_vec()));
    value::array(parts)
}

/// Where `needle` first occurs in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `join(array, delimiter[, null_text])` of strings: a null element is left out, or replaced
/// by `null_text`; any other element, or bytes to join with, cannot be joined.
fn join(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let array = match &values[0] {
        None => None,
        Some(v) if value::is_null(v) => None,
        Some(Value {
            value_type: Some(ValueType::ArrayValue(array)),
        }) => Some(&array.values),
        Some(other) => return Err(requires("join", "Array", other)),
    };
    // The delimiter and the null text, with bytes as `None`.
    let mut texts = Vec::with_capacity(2);
    let mut null = array.is_none();
    for v in &values[1..] {
        match v.as_ref().and_then(|v| v.value_type.as_ref()) {
            None | Some(ValueType::NullValue(_)) => null = true,
            Some(ValueType::StringValue(s)) => texts.push(Some(s.as_str())),
            Some(ValueType::BytesValue(_)) => texts.push(None),
            Some(_) => return Err(requires("join", "String", v.as_ref().expect("a value"))),
        }
    }
    let Some(array) = array.filter(|_| !null) else {
        return super::null();
    };
    let non_string = || invalid("Cannot join non-string types.");
    let Some(texts) = texts.into_iter().collect::<Option<Vec<&str>>>() else {
        return Err(non_string());
    };
    let (delimiter, null_text) = (texts[0], texts.get(1).copied());
    let mut parts = Vec::with_capacity(array.len());
    for v in array {
        match &v.value_type {
            Some(ValueType::StringValue(s)) => parts.push(s.as_str()),
            None | Some(ValueType::NullValue(_)) => parts.extend(null_text),
            Some(_) => return Err(non_string()),
        }
    }
    Ok(Some(value::string(parts.join(delimiter))))
}

/// `string_repeat(input, repetitions)`.
fn repeat(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let input = texts("string_repeat", EITHER, &values[..1])?;
    let repetitions = integer("string_repeat", values[1].as_ref())?;
    let (Some(input), Some(repetitions)) = (input, repetitions) else {
        return super::null();
    };
    let repetitions = usize::try_from(repetitions)
        .map_err(|_| invalid("string_repeat(...) repetition must be non-negative"))?;
    let len = match &input {
        Texts::Strings(s) => s[0].len(),
        Texts::Bytes(b) => b[0].len(),
    };
    if len
        .checked_mul(repetitions)
        .is_none_or(|total| total > MAX_REPEAT)
    {
        return Err(invalid("string_repeat(...) result is too large"));
    }
    Ok(Some(match input {
        Texts::Strings(s) => value::string(s[0].repeat(repetitions)),
        Texts::Bytes(b) => value::bytes(b[0].repeat(repetitions)),
    }))
}

/// `string_replace_all` or (`once`) `string_replace_one`: an empty `find` replaces nothing.
fn replace(texts: &Texts<'_>, once: bool) -> Value {
    match texts {
        Texts::Strings(s) => {
            let (input, find, with) = (s[0], s[1], s[2]);
            value::string(match (find.is_empty(), once) {
                (true, _) => input.to_owned(),
                (false, true) => input.replacen(find, with, 1),
                (false, false) => input.replace(find, with),
            })
        }
        Texts::Bytes(b) => {
            let (mut rest, find, with) = (b[0], b[1], b[2]);
            let mut out = Vec::with_capacity(rest.len());
            if !find.is_empty() {
                while let Some(at) = find_bytes(rest, find) {
                    out.extend_from_slice(&rest[..at]);
                    out.extend_from_slice(with);
                    rest = &rest[at + find.len()..];
                    if once {
                        break;
                    }
                }
            }
            out.extend_from_slice(rest);
            value::bytes(out)
        }
    }
}

/// What a pattern is compiled for.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Use {
    /// An RE2 pattern, to match anywhere in a string.
    Search,
    /// An RE2 pattern, to match a whole string.
    Whole,
    /// A `like` pattern.
    Like,
}

/// `pattern` compiled for `what` by `compile`, or as it was for an earlier row.
fn compiled(
    pattern: &str,
    what: Use,
    compile: impl FnOnce() -> Result<Regex, Status>,
) -> Result<Regex, Status> {
    static COMPILED: LazyLock<Mutex<HashMap<(String, Use), Regex>>> = LazyLock::new(Mutex::default);
    let key = (pattern.to_owned(), what);
    if let Some(re) = COMPILED.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(re);
    }
    let re = compile()?;
    if let Ok(mut compiled) = COMPILED.lock() {
        if compiled.len() >= 64 {
            compiled.clear();
        }
        compiled.insert(key, re.clone());
    }
    Ok(re)
}

/// A SQL `LIKE` pattern as a regular expression of a whole string: `%` is any characters, `_`
/// one, and `\` makes the character after it literal.
fn like(pattern: &str) -> Result<Regex, Status> {
    compiled(pattern, Use::Like, || {
        let mut source = String::from(r"(?s)\A");
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            let literal = match c {
                '%' => {
                    source.push_str(".*");
                    continue;
                }
                '_' => {
                    source.push('.');
                    continue;
                }
                '\\' => chars.next().unwrap_or('\\'),
                c => c,
            };
            source.push_str(&regex::escape(literal.encode_utf8(&mut [0; 4])));
        }
        source.push_str(r"\z");
        Regex::new(&source).map_err(|error| syntax_error(&source, &error))
    })
}

/// The RE2 `pattern`, compiled to match anywhere or (`whole`) the whole string.
fn regex(pattern: &str, whole: bool) -> Result<Regex, Status> {
    compiled(
        pattern,
        if whole { Use::Whole } else { Use::Search },
        || {
            let source = rust_syntax(pattern);
            let re = Regex::new(&source).map_err(|error| match Regex::new(pattern) {
                // Where the pattern as written is wrong, RE2 says so.
                Err(error) => syntax_error(pattern, &error),
                Ok(_) => syntax_error(&source, &error),
            })?;
            if !whole {
                return Ok(re);
            }
            Regex::new(&format!(r"\A(?:{source})\z")).map_err(|error| syntax_error(&source, &error))
        },
    )
}

/// The pattern of `regex_find` or `regex_find_all`: with at most one capture group, which
/// gives what they extract.
fn extraction(pattern: &str) -> Result<Regex, Status> {
    let re = regex(pattern, false)?;
    if re.captures_len() > 2 {
        return Err(invalid("At most one capture group is supported."));
    }
    Ok(re)
}

/// What a match of `re` extracts: its capture group if it has one (null when that did not
/// take part), else the whole match.
fn extracted(re: &Regex, found: &regex::Captures<'_>) -> Value {
    found
        .get(re.captures_len() - 1)
        .map_or_else(value::null, |m| value::string(m.as_str()))
}

/// An RE2 pattern in the `regex` crate's syntax: `\d`, `\w`, `\s` and `\b` are ASCII-only in
/// RE2, and a `[` (unless it opens `[:alpha:]`), `&` or `~` inside a class is a literal.
fn rust_syntax(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut chars = pattern.chars().peekable();
    // Inside a class, and whether the next character is its first.
    let mut class = false;
    let mut first = false;
    while let Some(c) = chars.next() {
        let at_first = std::mem::take(&mut first);
        match c {
            '\\' => {
                let Some(e) = chars.next() else {
                    out.push('\\');
                    break;
                };
                out.push_str(match (class, e) {
                    (false, 'd') => "[0-9]",
                    (false, 'D') => "[^0-9]",
                    (false, 'w') => "[0-9A-Za-z_]",
                    (false, 'W') => "[^0-9A-Za-z_]",
                    (false, 's') => r"[\t\n\f\r ]",
                    (false, 'S') => r"[^\t\n\f\r ]",
                    (false, 'b') => r"(?-u:\b)",
                    (false, 'B') => r"(?-u:\B)",
                    (true, 'd') => "0-9",
                    (true, 'w') => "0-9A-Za-z_",
                    (true, 's') => r"\t\n\f\r ",
                    (true, 'D') => "[^0-9]",
                    (true, 'W') => "[^0-9A-Za-z_]",
                    (true, 'S') => r"[^\t\n\f\r ]",
                    _ => {
                        out.push('\\');
                        out.push(e);
                        continue;
                    }
                });
            }
            '[' if !class => {
                out.push('[');
                class = true;
                if chars.peek() == Some(&'^') {
                    out.push('^');
                    chars.next();
                }
                first = true;
            }
            // `[]a]`: a bracket first in a class is a literal.
            ']' if class && at_first => out.push_str(r"\]"),
            ']' if class => {
                out.push(']');
                class = false;
            }
            '[' if class && chars.peek() == Some(&':') => {
                out.push('[');
                for c in chars.by_ref() {
                    out.push(c);
                    if c == ']' {
                        break;
                    }
                }
            }
            '[' | '&' | '~' if class => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// RE2's error for a pattern the `regex` crate refuses: its message for what the crate says is
/// wrong, and the part of the pattern RE2 shows, from where the crate points.
fn syntax_error(pattern: &str, error: &regex::Error) -> Status {
    let message = error.to_string();
    let what = match error {
        regex::Error::CompiledTooBig(_) => "expression too large",
        _ => message
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("error: "))
            .unwrap_or(&message),
    };
    // A one-line pattern comes with a line of `^` under the characters at fault: the marked
    // characters, and those from the first of them on.
    let (start, len) = message
        .lines()
        .nth(2)
        .filter(|_| !pattern.contains('\n'))
        .and_then(|carets| {
            let at = carets.find('^')?;
            let len = carets[at..].chars().take_while(|&c| c == '^').count();
            Some((at.checked_sub(4)?, len))
        })
        .unwrap_or((0, usize::MAX));
    let marked: String = pattern.chars().skip(start).take(len).collect();
    let from_marked: String = pattern.chars().skip(start).collect();
    let (code, expr) = match what {
        "unclosed group" => ("missing closing )", pattern.to_owned()),
        "unopened group" => ("unexpected )", pattern.to_owned()),
        "unclosed character class" => ("missing closing ]", from_marked),
        "repetition operator missing expression" => {
            ("missing argument to repetition operator", marked)
        }
        "unrecognized escape sequence"
        | "invalid escape sequence found in character class"
        | "backreferences are not supported" => ("invalid escape sequence", marked),
        "incomplete escape sequence, reached end of pattern prematurely" => {
            ("trailing backslash at end of expression", String::new())
        }
        "invalid character class range, the start must be <= the end"
        | "Unicode property not found" => ("invalid character class range", marked),
        "invalid repetition count range, the start must be <= the end" => {
            ("invalid repeat count", marked)
        }
        "look-around, including look-ahead and look-behind, is not supported" => {
            ("invalid or unsupported Perl syntax", marked)
        }
        "expression too large" => (what, pattern.to_owned()),
        _ => (what, marked),
    };
    Status::invalid_argument(format!(
        "Unsupported regex syntax: error parsing regexp: {code}: `{expr}`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(pattern: &str) -> String {
        regex(pattern, false).unwrap_err().message().to_owned()
    }

    #[test]
    fn refuses_patterns_as_re2_does() {
        let prefix = "Unsupported regex syntax: error parsing regexp: ";
        for (pattern, message) in [
            ("(", "missing closing ): `(`"),
            ("a(b", "missing closing ): `a(b`"),
            ("a)b", "unexpected ): `a)b`"),
            ("[", "missing closing ]: `[`"),
            ("x[ab", "missing closing ]: `[ab`"),
            ("*", "missing argument to repetition operator: `*`"),
            (r"\q", r"invalid escape sequence: `\q`"),
            (r"a\", "trailing backslash at end of expression: ``"),
            ("[z-a]", "invalid character class range: `z-a`"),
            ("a{2,1}", "invalid repeat count: `{2,1}`"),
            ("(?=a)", "invalid or unsupported Perl syntax: `(?=`"),
        ] {
            assert_eq!(refusal(pattern), format!("{prefix}{message}"), "{pattern}");
        }
        // Checked before it is wrapped to match a whole string, where it would be valid.
        assert!(regex("a)(b", true).is_err());
    }

    #[test]
    fn perl_classes_are_ascii() {
        assert!(!regex(r"\d", false).unwrap().is_match("٣"));
        assert!(regex(r"[\d]", false).unwrap().is_match("3"));
        assert!(!regex(r"\w", false).unwrap().is_match("é"));
        assert!(regex(r"[^\D]", false).unwrap().is_match("7"));
        assert!(regex(r"\bb", false).unwrap().is_match("éb"));
        assert!(regex("[a[b]", false).unwrap().is_match("["));
    }

    #[test]
    fn like_patterns() {
        let like = |pattern: &str, s: &str| like(pattern).unwrap().is_match(s);
        assert!(like("Hello%", "Hello World"));
        assert!(like("_ello World", "Hello World"));
        assert!(!like("hello%", "Hello World"));
        assert!(like("%o W%", "Hello World"));
        assert!(like("a%b", "a\nb"));
        assert!(like(r"100\%", "100%"));
        assert!(!like(r"100\%", "1000"));
        assert!(like("a.c", "a.c"));
        assert!(!like("a.c", "abc"));
    }
}
