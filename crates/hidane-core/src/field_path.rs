//! Field paths (`a.b`, `` `x.y`.z ``) and the operations masks need on document fields.
//!
//! Syntax, as the official emulator enforces it: segments are separated by `.`; an unquoted
//! segment matches `[a-zA-Z_][a-zA-Z_0-9]*`; any other segment is wrapped in backticks, where
//! `` \` `` and `\\` are escapes.

use std::fmt;

use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};
pub use hidane_proto::ordered::Fields;

use crate::{hash_order::hash_order, order::is_vector, store::identical};

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

/// The value at `path`, if every segment but the last names a map. A vector is one value, not
/// a map: no path goes into it, as on the official emulator (#262).
pub fn get<'a>(fields: &'a Fields, path: &FieldPath) -> Option<&'a Value> {
    let (last, parents) = path.0.split_last()?;
    let mut current = fields;
    for segment in parents {
        current = map_fields(current.get(segment)?)?;
    }
    current.get(last)
}

/// Sets `path` to `value`, creating (or replacing non-map values, vectors among them, with)
/// intermediate maps.
pub fn set(fields: &mut Fields, path: &FieldPath, value: Value) {
    let Some((last, parents)) = path.0.split_last() else {
        return;
    };
    let mut current = fields;
    for segment in parents {
        let entry = current.entry(segment.clone()).or_insert_with(empty_map);
        if map_fields(entry).is_none() {
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
            Some(ValueType::MapValue(map)) if !is_vector(map) => current = &mut map.fields,
            _ => return,
        }
    }
    current.remove(last);
}

/// Masked paths as a tree of segments. Each level lists its names in the order they first
/// appear in the mask.
#[derive(Default)]
struct MaskTree {
    /// The mask names this exact path: its whole value is taken.
    whole: bool,
    children: Vec<(String, MaskTree)>,
}

impl MaskTree {
    fn new(paths: &[FieldPath]) -> Self {
        let mut root = Self::default();
        for path in paths {
            let mut node = &mut root;
            for segment in path.segments() {
                let at = match node.children.iter().position(|(name, _)| name == segment) {
                    Some(at) => at,
                    None => {
                        node.children.push((segment.clone(), Self::default()));
                        node.children.len() - 1
                    }
                };
                node = &mut node.children[at].1;
            }
            node.whole = true;
        }
        root
    }

    /// The names of this level in the order the official emulator visits them: the order of
    /// a Java `HashMap` that received them in mask order.
    fn ordered(&self) -> impl Iterator<Item = &(String, MaskTree)> {
        let names: Vec<&str> = self
            .children
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        hash_order(&names).into_iter().map(|i| &self.children[i])
    }

    /// The paths this level takes whole, below `prefix`, in the order rules list them: that
    /// of a Java `HashMap` that received the names taken whole first, then the others, each
    /// in mask order.
    fn flatten(&self, prefix: &mut Vec<String>, out: &mut Vec<FieldPath>) {
        let (whole, below): (Vec<_>, Vec<_>) =
            self.children.iter().partition(|(_, node)| node.whole);
        let children: Vec<&(String, MaskTree)> = whole.into_iter().chain(below).collect();
        let names: Vec<&str> = children.iter().map(|(name, _)| name.as_str()).collect();
        for (name, below) in hash_order(&names).into_iter().map(|i| children[i]) {
            prefix.push(name.clone());
            if below.whole {
                out.push(FieldPath(prefix.clone()));
            } else {
                below.flatten(prefix, out);
            }
            prefix.pop();
        }
    }
}

/// The paths of a mask as the official emulator lists them to Security Rules
/// (`request.writeFields`, `request.fields`): each once, without those below another, and at
/// each level in the order of a Java `HashMap` that received the names the mask takes whole,
/// then the others (`tools/oracle/rules_request_fields.py`).
pub fn mask_paths(mask: &[FieldPath]) -> Vec<FieldPath> {
    let mut out = Vec::new();
    MaskTree::new(mask).flatten(&mut Vec::new(), &mut out);
    out
}

/// `entries` in the order of a Java `HashMap` that received them in turn.
fn in_hash_order<T>(mut entries: Vec<(String, T)>) -> impl Iterator<Item = (String, T)> {
    let order = hash_order(&entries.iter().map(|(name, _)| name).collect::<Vec<_>>());
    let mut taken: Vec<Option<(String, T)>> = entries.drain(..).map(Some).collect();
    order.into_iter().filter_map(move |i| taken[i].take())
}

/// The fields of a map a path can go into: not those of a vector, which is one value.
fn map_fields(value: &Value) -> Option<&Fields> {
    match &value.value_type {
        Some(ValueType::MapValue(map)) if !is_vector(map) => Some(&map.fields),
        _ => None,
    }
}

fn map_of(fields: Fields) -> Value {
    Value {
        value_type: Some(ValueType::MapValue(MapValue { fields })),
    }
}

/// Keeps only the fields named by `paths` (a read mask). As on the official emulator, each map
/// lists what it returns in the order of a Java `HashMap`: the masked names are visited in the
/// order of one, and the values found are put into another (#108).
pub fn project(fields: &Fields, paths: &[FieldPath]) -> Fields {
    project_level(fields, &MaskTree::new(paths), false)
}

/// [`project`] as an enterprise database projects a query's documents: every map on the way
/// to a selected path is there, empty where the document has nothing below it, whatever the
/// document holds there (nothing, a value that is not a map, a vector, whose inside no path
/// reaches).
pub fn project_keeping_maps(fields: &Fields, paths: &[FieldPath]) -> Fields {
    project_level(fields, &MaskTree::new(paths), true)
}

fn project_level(fields: &Fields, mask: &MaskTree, keep_maps: bool) -> Fields {
    let mut found = Vec::new();
    for (name, below) in mask.ordered() {
        let value = fields.get(name);
        if below.whole {
            if let Some(value) = value {
                found.push((name.clone(), value.clone()));
            }
        } else if let Some(map) = value.and_then(map_fields) {
            let inner = project_level(map, below, keep_maps);
            if keep_maps || !inner.is_empty() {
                found.push((name.clone(), map_of(inner)));
            }
        } else if keep_maps {
            let inner = project_level(&Fields::default(), below, keep_maps);
            found.push((name.clone(), map_of(inner)));
        }
    }
    in_hash_order(found).collect()
}

/// Applies an update mask: every masked path takes its value from `input`, or is deleted when
/// `input` has no value there. Fields outside the mask are untouched, including fields of
/// `input` that the mask does not name.
///
/// Where fields land follows the official emulator (#108): at each level, the masked names are
/// visited in the order of a Java `HashMap`, those whose value changes are put into another
/// one, and in that map's order each is removed and, unless deleted, added again at the end.
/// The names the mask takes whole go into that map before those it masks a path below, which
/// decides the order of two names in one bucket (`fo-bucket` in `tools/oracle/field_order.py`).
/// A map changed inside moves the same way; fields that keep their value stay where they are.
pub fn apply_mask(existing: &mut Fields, input: &Fields, mask: &[FieldPath]) {
    merge_level(existing, Some(input), &MaskTree::new(mask));
}

fn merge_level(existing: &mut Fields, input: Option<&Fields>, mask: &MaskTree) {
    let mut changes = Vec::new();
    let (whole, nested): (Vec<_>, Vec<_>) = mask.ordered().partition(|(_, below)| below.whole);
    for (name, below) in whole.into_iter().chain(nested) {
        let old = existing.get(name);
        let given = input.and_then(|fields| fields.get(name));
        let new = if below.whole {
            given.cloned()
        } else {
            let mut inner = old.and_then(map_fields).cloned().unwrap_or_default();
            merge_level(&mut inner, given.and_then(map_fields), below);
            match old {
                // A map, or something the masked paths below turn into one.
                Some(old) if map_fields(old).is_some() || !inner.is_empty() => Some(map_of(inner)),
                Some(old) => Some(old.clone()),
                None if inner.is_empty() => None,
                None => Some(map_of(inner)),
            }
        };
        // Bit for bit: `-0.0` over `0.0` is a change, NaN over NaN is not.
        let changed = match (&new, old) {
            (Some(new), Some(old)) => !identical(new, old),
            (new, old) => new.is_some() || old.is_some(),
        };
        if changed {
            changes.push((name.clone(), new));
        }
    }
    for (name, new) in in_hash_order(changes) {
        existing.remove(&name);
        if let Some(value) = new {
            existing.insert(name, value);
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

    #[test]
    fn mask_paths_as_rules_see_them() {
        let listed = |mask: &[&str]| -> Vec<String> {
            let mask: Vec<FieldPath> = mask.iter().map(|p| FieldPath::parse(p).unwrap()).collect();
            mask_paths(&mask).iter().map(ToString::to_string).collect()
        };
        // Recorded in crates/hidane/tests/fixtures/rules_request_fields.json.
        assert_eq!(listed(&["c", "b", "a"]), ["a", "b", "c"]);
        assert_eq!(listed(&["n", "m.b", "z"]), ["z", "m.b", "n"]);
        assert_eq!(listed(&["b", "a", "b"]), ["a", "b"]);
        assert_eq!(listed(&["m.b", "m"]), ["m"]);
        assert_eq!(listed(&["m.b.c", "m.a", "z"]), ["z", "m.a", "m.b.c"]);
        assert_eq!(
            listed(&["x.y", "a", "`q r`", "b.c.d"]),
            ["a", "`q r`", "b.c.d", "x.y"]
        );
        // `b`, `r`, `B` and `R` share a bucket: names taken whole come first.
        assert_eq!(listed(&["b.c", "r", "B.x", "R"]), ["r", "R", "b.c", "B.x"]);
        assert_eq!(listed(&["m.b.c", "m.r"]), ["m.r", "m.b.c"]);
        assert!(listed(&[]).is_empty());
    }

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
    fn enterprise_projections_keep_the_maps_on_the_way() {
        let doc = fields(&[("a", int(1)), ("b", map(&[("x", int(1)), ("y", int(2))]))]);
        let empty = || map(&[]);
        // Recorded in crates/hidane/tests/fixtures/queries_enterprise.json.
        assert_eq!(
            project_keeping_maps(
                &doc,
                &[fp("b.x"), fp("missing"), fp("a.not_a_map"), fp("m.x.y")]
            ),
            project_keeping_maps(
                &fields(&[
                    ("b", map(&[("x", int(1))])),
                    ("a", empty()),
                    ("m", map(&[("x", empty())])),
                ]),
                &[fp("b"), fp("a"), fp("m")]
            )
        );
    }

    /// The names of `fields`, nested maps as `name{...}`, in the order they are held.
    fn layout(fields: &Fields) -> String {
        fields
            .iter()
            .map(|(name, value)| match &value.value_type {
                Some(ValueType::MapValue(map)) => format!("{name}{{{}}}", layout(&map.fields)),
                _ => name.clone(),
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    // The cases below are recorded from the official emulator in
    // crates/hidane/tests/fixtures/field_order.json (#108).

    #[test]
    fn masked_fields_that_change_move_to_the_end_in_hash_order() {
        let base = || {
            fields(&[
                ("zeta", int(1)),
                ("alpha", int(2)),
                ("mid", map(&[("z", int(3)), ("a", int(4))])),
            ])
        };
        let mut doc = base();
        apply_mask(
            &mut doc,
            &fields(&[("beta", int(9)), ("alpha", int(10))]),
            &[fp("beta"), fp("alpha")],
        );
        assert_eq!(layout(&doc), "zeta,mid{z,a},alpha,beta");

        // alpha keeps its value, so it stays.
        let mut doc = base();
        apply_mask(
            &mut doc,
            &fields(&[
                ("deep", map(&[("x", map(&[("y", int(1))]))])),
                ("alpha", int(2)),
            ]),
            &[fp("deep.x.y"), fp("alpha")],
        );
        assert_eq!(layout(&doc), "zeta,alpha,mid{z,a},deep{x{y}}");
    }

    #[test]
    fn a_map_changed_inside_moves_to_the_end() {
        let base = || {
            fields(&[
                ("mid", map(&[("z", int(3)), ("a", int(4))])),
                ("zeta", int(1)),
                ("alpha", int(2)),
            ])
        };
        let mut doc = base();
        apply_mask(
            &mut doc,
            &fields(&[("mid", map(&[("b", int(5))]))]),
            &[fp("mid.b")],
        );
        assert_eq!(layout(&doc), "zeta,alpha,mid{z,a,b}");

        let mut doc = base();
        apply_mask(&mut doc, &Fields::new(), &[fp("mid.z")]);
        assert_eq!(layout(&doc), "zeta,alpha,mid{a}");

        let mut doc = fields(&[
            ("p", map(&[("x", int(1)), ("y", int(2))])),
            ("alpha", int(1)),
            ("zeta", int(2)),
            ("mid", int(3)),
        ]);
        apply_mask(
            &mut doc,
            &fields(&[("alpha", int(9))]),
            &[fp("p.y"), fp("alpha")],
        );
        assert_eq!(layout(&doc), "zeta,mid,p{x},alpha");
    }

    #[test]
    fn the_changed_fields_have_a_hash_map_of_their_own() {
        let present = [
            "str", "int", "dbl", "zeta", "alpha", "a", "b", "c", "x1", "x2", "x3",
        ];
        let doc_with = |extra: &[(&str, Value)]| {
            let mut doc = fields(&[("keep", int(0))]);
            for (n, name) in present.iter().enumerate() {
                doc.insert((*name).to_owned(), int(n as i64));
            }
            doc.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
            doc
        };
        let input: Fields = present
            .iter()
            .enumerate()
            .map(|(n, name)| ((*name).to_owned(), int(n as i64 + 100)))
            .collect();
        let mask: Vec<FieldPath> = present
            .iter()
            .chain(&["gone1", "gone2"])
            .map(|p| fp(p))
            .collect();

        // Thirteen masked names, eleven changes: the changes fit 16 buckets.
        let mut doc = doc_with(&[]);
        apply_mask(&mut doc, &input, &mask);
        assert_eq!(layout(&doc), "keep,zeta,str,a,b,c,alpha,x1,x2,x3,int,dbl");

        // Deleting two fields makes thirteen changes: 32 buckets.
        let mut doc = doc_with(&[("gone1", int(1)), ("gone2", int(2))]);
        apply_mask(&mut doc, &input, &mask);
        assert_eq!(layout(&doc), "keep,zeta,a,b,c,int,dbl,str,alpha,x1,x2,x3");
    }

    #[test]
    fn read_masks_return_fields_in_hash_order() {
        let doc = fields(&[
            ("str", int(1)),
            ("mid", map(&[("z", int(1)), ("a", int(2))])),
            ("zeta", int(3)),
            ("alpha", int(4)),
            ("a", int(5)),
        ]);
        let masked = |mask: &[&str]| {
            layout(&project(
                &doc,
                &mask.iter().map(|p| fp(p)).collect::<Vec<_>>(),
            ))
        };
        assert_eq!(masked(&["a", "alpha", "zeta"]), "zeta,a,alpha");
        assert_eq!(masked(&["mid.a", "mid.z", "str"]), "str,mid{a,z}");
        // A masked map keeps its own order.
        assert_eq!(masked(&["mid", "str"]), "str,mid{z,a}");
    }

    #[test]
    fn set_replaces_non_map_intermediates() {
        let mut doc = fields(&[("a", int(1))]);
        set(&mut doc, &fp("a.b"), int(2));
        assert_eq!(doc, fields(&[("a", map(&[("b", int(2))]))]));
    }
}
