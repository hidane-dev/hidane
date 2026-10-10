//! Field paths (`a.b`, `` `x.y`.z ``) and the operations masks need on document fields.
//!
//! Syntax, as the official emulator enforces it: segments are separated by `.`; an unquoted
//! segment matches `[a-zA-Z_][a-zA-Z_0-9]*`; any other segment is wrapped in backticks, where
//! `` \` `` and `\\` are escapes.

use std::{collections::BTreeMap, fmt};

use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};

pub type Fields = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    /// Parses a field path; the error is the official emulator's message.
    pub fn parse(path: &str) -> Result<Self, String> {
        let invalid = || {
            format!(
                "Invalid property path \"{path}\". Unquoted property paths must match regex \
                 ([a-zA-Z_][a-zA-Z_0-9]*), and quoted property paths must match regex \
                 (`(?:[^`\\\\]|(?:\\\\.))+`)"
            )
        };
        let mut segments = Vec::new();
        let mut chars = path.chars().peekable();
        loop {
            let mut segment = String::new();
            if chars.peek() == Some(&'`') {
                chars.next();
                loop {
                    match chars.next() {
                        Some('`') => break,
                        Some('\\') => segment.push(chars.next().ok_or_else(invalid)?),
                        Some(c) => segment.push(c),
                        None => return Err(invalid()),
                    }
                }
                if segment.is_empty() {
                    return Err(invalid());
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == '.' {
                        break;
                    }
                    let valid = if segment.is_empty() {
                        c.is_ascii_alphabetic() || c == '_'
                    } else {
                        c.is_ascii_alphanumeric() || c == '_'
                    };
                    if !valid {
                        return Err(invalid());
                    }
                    segment.push(c);
                    chars.next();
                }
                if segment.is_empty() {
                    return Err(invalid());
                }
            }
            segments.push(segment);
            match chars.next() {
                None => return Ok(Self(segments)),
                Some('.') => {}
                Some(_) => return Err(invalid()),
            }
        }
    }

    pub fn from_segments<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(segments.into_iter().map(Into::into).collect())
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

/// The canonical form: simple segments as they are, others in backticks with `` ` `` and `\`
/// escaped, joined by `.`.
impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            let mut chars = segment.chars();
            let simple = chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
            if simple {
                f.write_str(segment)?;
            } else {
                f.write_str("`")?;
                for c in segment.chars() {
                    if c == '`' || c == '\\' {
                        f.write_str("\\")?;
                    }
                    write!(f, "{c}")?;
                }
                f.write_str("`")?;
            }
        }
        Ok(())
    }
}

/// The value at `path`, if every segment but the last names a map.
pub fn get<'a>(fields: &'a Fields, path: &FieldPath) -> Option<&'a Value> {
    let (last, parents) = path.0.split_last()?;
    let mut current = fields;
    for segment in parents {
        match &current.get(segment)?.value_type {
            Some(ValueType::MapValue(map)) => current = &map.fields,
            _ => return None,
        }
    }
    current.get(last)
}

/// Sets `path` to `value`, creating (or replacing non-map values with) intermediate maps.
pub fn set(fields: &mut Fields, path: &FieldPath, value: Value) {
    let Some((last, parents)) = path.0.split_last() else {
        return;
    };
    let mut current = fields;
    for segment in parents {
        let entry = current.entry(segment.clone()).or_insert_with(empty_map);
        if !matches!(entry.value_type, Some(ValueType::MapValue(_))) {
            *entry = empty_map();
        }
        let Some(ValueType::MapValue(map)) = &mut entry.value_type else {
            unreachable!("just made it a map");
        };
        current = &mut map.fields;
    }
    current.insert(last.clone(), value);
}

/// Removes `path` if it exists. Intermediate maps are kept even if they become empty.
pub fn delete(fields: &mut Fields, path: &FieldPath) {
    let Some((last, parents)) = path.0.split_last() else {
        return;
    };
    let mut current = fields;
    for segment in parents {
        match current.get_mut(segment).and_then(|v| v.value_type.as_mut()) {
            Some(ValueType::MapValue(map)) => current = &mut map.fields,
            _ => return,
        }
    }
    current.remove(last);
}

/// Keeps only the fields named by `paths` (a read mask).
pub fn project(fields: &Fields, paths: &[FieldPath]) -> Fields {
    let mut out = Fields::new();
    for path in paths {
        if let Some(value) = get(fields, path) {
            set(&mut out, path, value.clone());
        }
    }
    out
}

/// Applies an update mask: every masked path takes its value from `input`, or is deleted when
/// `input` has no value there. Fields outside the mask are untouched, including fields of
/// `input` that the mask does not name.
pub fn apply_mask(existing: &mut Fields, input: &Fields, mask: &[FieldPath]) {
    for path in mask {
        match get(input, path) {
            Some(value) => set(existing, path, value.clone()),
            None => delete(existing, path),
        }
    }
}

fn empty_map() -> Value {
    Value {
        value_type: Some(ValueType::MapValue(MapValue::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(i: i64) -> Value {
        Value {
            value_type: Some(ValueType::IntegerValue(i)),
        }
    }

    fn map(entries: &[(&str, Value)]) -> Value {
        Value {
            value_type: Some(ValueType::MapValue(MapValue {
                fields: entries
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            })),
        }
    }

    fn fields(entries: &[(&str, Value)]) -> Fields {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn fp(s: &str) -> FieldPath {
        FieldPath::parse(s).unwrap()
    }

    #[test]
    fn display_is_the_canonical_form() {
        for canonical in [
            "a", "a.b", "_x9", "`a b`.c", "`1a`", "`x\\`y`", "`a\\\\b`", "`é`",
        ] {
            assert_eq!(fp(canonical).to_string(), canonical);
        }
        assert_eq!(fp("`a`.`b`").to_string(), "a.b");
    }

    #[test]
    fn parses_unquoted_and_quoted_segments() {
        assert_eq!(fp("a").segments(), ["a"]);
        assert_eq!(fp("a.b_1._c").segments(), ["a", "b_1", "_c"]);
        assert_eq!(fp("`a.b`.c").segments(), ["a.b", "c"]);
        assert_eq!(fp("`x\\`y`").segments(), ["x`y"]);
        assert_eq!(fp("`\\\\`").segments(), ["\\"]);
        assert_eq!(fp("`1st`").segments(), ["1st"]);
    }

    #[test]
    fn rejects_like_the_official_emulator() {
        for bad in [
            "", "a..b", ".a", "a.", "1a", "a-b", "``", "`a", "`a`b", "a b",
        ] {
            let err = FieldPath::parse(bad).unwrap_err();
            assert!(
                err.starts_with(&format!("Invalid property path \"{bad}\". Unquoted")),
                "{bad}: {err}"
            );
        }
        // The exact text the official emulator returns (tests/fixtures/document_writes.json).
        assert_eq!(
            FieldPath::parse("a..b").unwrap_err(),
            "Invalid property path \"a..b\". Unquoted property paths must match regex \
             ([a-zA-Z_][a-zA-Z_0-9]*), and quoted property paths must match regex \
             (`(?:[^`\\\\]|(?:\\\\.))+`)"
        );
    }

    #[test]
    fn update_mask_matches_the_official_emulator() {
        // Observed: {a:1, b:2, c:{x:1, y:2}} patched with mask [a, c.x, d] and input
        // {a:10, c:{x:5}, e:9} becomes {a:10, b:2, c:{x:5, y:2}}.
        let mut doc = fields(&[
            ("a", int(1)),
            ("b", int(2)),
            ("c", map(&[("x", int(1)), ("y", int(2))])),
        ]);
        let input = fields(&[("a", int(10)), ("c", map(&[("x", int(5))])), ("e", int(9))]);
        apply_mask(&mut doc, &input, &[fp("a"), fp("c.x"), fp("d")]);
        assert_eq!(
            doc,
            fields(&[
                ("a", int(10)),
                ("b", int(2)),
                ("c", map(&[("x", int(5)), ("y", int(2))])),
            ])
        );
    }

    #[test]
    fn masks_on_missing_documents_create_only_the_masked_fields() {
        let mut doc = Fields::new();
        let input = fields(&[("a", int(1)), ("b", map(&[("x", int(1))]))]);
        apply_mask(&mut doc, &input, &[fp("b.x")]);
        assert_eq!(doc, fields(&[("b", map(&[("x", int(1))]))]));
    }

    #[test]
    fn read_masks_project_nested_fields() {
        let doc = fields(&[
            ("a", int(1)),
            ("b", map(&[("x", int(1)), ("y", int(2))])),
            ("c", int(3)),
        ]);
        assert_eq!(
            project(
                &doc,
                &[fp("b.x"), fp("c"), fp("missing"), fp("a.not_a_map")]
            ),
            fields(&[("b", map(&[("x", int(1))])), ("c", int(3))])
        );
    }

    #[test]
    fn set_replaces_non_map_intermediates() {
        let mut doc = fields(&[("a", int(1))]);
        set(&mut doc, &fp("a.b"), int(2));
        assert_eq!(doc, fields(&[("a", map(&[("b", int(2))]))]));
    }
}
