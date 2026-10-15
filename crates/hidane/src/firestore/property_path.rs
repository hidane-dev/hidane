//! Field paths as the official emulator reads them, in its passes (`tools/oracle/rest.py`,
//! `tools/oracle/queries.py`, `tools/oracle/aggregations.py`, #262).
//!
//! A path is read first, the same way wherever it is ([`PropertyPath::read`]): segments
//! separated by `.`, each a name (`[a-zA-Z_][a-zA-Z_0-9]*`, or anything in backticks with
//! `` \` `` and `\\` escaped) followed by any number of array offsets (`a[0]`, `a[0][1]`). A
//! name `__key__` is refused as soon as it is read, before the rest of the path; `__name__`
//! stands for `__key__` from then on. Once the path is read, a reserved name other than
//! `__key__` and `__type__` (`__x__`) is refused with the rest of the path.
//!
//! What may hold array offsets, `__key__` and `__type__` depends on where the path is, and is
//! checked later, segment by segment: masks and projections ([`PropertyPath::check_mask`]),
//! filters, orders and aggregations ([`PropertyPath::check_query`]), transforms
//! ([`PropertyPath::check_transform`]).

use std::fmt;

use hidane_core::field_path::FieldPath;
use tonic::Status;

/// One segment: a name, and the array offsets after it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Segment {
    /// As written, unquoted (`__name__` stays `__name__`).
    pub name: String,
    pub offsets: Vec<u32>,
}

impl Segment {
    /// `__name__`, which stands for the key.
    fn is_key(&self) -> bool {
        self.name == "__name__"
    }

    /// The name as the official emulator's messages print it.
    fn shown(&self) -> &str {
        if self.is_key() { "__key__" } else { &self.name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PropertyPath(Vec<Segment>);

const MAX_OFFSET: u64 = (1 << 31) - 1;

/// Whether `path` has a field path's syntax, names and array offsets read as
/// [`PropertyPath::read`] reads them but not checked: what an order of ListDocuments takes as
/// its path, anything else being an invalid clause (`tools/oracle/rest.py`, #262).
pub fn well_formed(path: &str) -> bool {
    let mut chars = path.chars().peekable();
    loop {
        if chars.next_if_eq(&'`').is_some() {
            let mut name = 0;
            loop {
                match chars.next() {
                    Some('`') => break,
                    Some('\\') if chars.next().is_none() => return false,
                    Some(_) => name += 1,
                    None => return false,
                }
            }
            if name == 0 {
                return false;
            }
        } else {
            if chars
                .next_if(|c| c.is_ascii_alphabetic() || *c == '_')
                .is_none()
            {
                return false;
            }
            while chars
                .next_if(|c| c.is_ascii_alphanumeric() || *c == '_')
                .is_some()
            {}
        }
        while chars.next_if_eq(&'[').is_some() {
            if chars.next_if(char::is_ascii_digit).is_none() {
                return false;
            }
            while chars.next_if(char::is_ascii_digit).is_some() {}
            if chars.next_if_eq(&']').is_none() {
                return false;
            }
        }
        match chars.next() {
            None => return true,
            Some('.') => {}
            Some(_) => return false,
        }
    }
}

/// Field names of the form `__…__` are reserved; `____` is not one.
pub fn reserved(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

fn invalid() -> Status {
    Status::invalid_argument("Invalid property path")
}

impl PropertyPath {
    /// Reads `path` (see the module): not empty, its syntax, its array offsets, no name
    /// `__key__`, then no reserved name but `__key__` and `__type__`.
    pub fn read(path: &str) -> Result<Self, Status> {
        let parsed = Self::parse(path)?;
        if let Some(at) = parsed
            .0
            .iter()
            .position(|s| reserved(s.shown()) && !matches!(s.shown(), "__key__" | "__type__"))
        {
            return Err(Status::invalid_argument(format!(
                "Invalid reserved name in field path {}",
                Shown(&parsed.0[at..])
            )));
        }
        Ok(parsed)
    }

    fn parse(path: &str) -> Result<Self, Status> {
        if path.is_empty() {
            return Err(Status::invalid_argument(
                "Invalid empty property path string.",
            ));
        }
        let syntax = || {
            Status::invalid_argument(format!(
                "Invalid property path \"{path}\". Unquoted property paths must match regex \
                 ([a-zA-Z_][a-zA-Z_0-9]*), and quoted property paths must match regex \
                 (`(?:[^`\\\\]|(?:\\\\.))+`)"
            ))
        };
        let mut segments = Vec::new();
        let mut chars = path.chars().peekable();
        loop {
            let mut name = String::new();
            if chars.peek() == Some(&'`') {
                chars.next();
                loop {
                    match chars.next() {
                        Some('`') => break,
                        Some('\\') => name.push(chars.next().ok_or_else(syntax)?),
                        Some(c) => name.push(c),
                        None => return Err(syntax()),
                    }
                }
                if name.is_empty() {
                    return Err(syntax());
                }
            } else {
                while let Some(&c) = chars.peek() {
                    let valid = if name.is_empty() {
                        c.is_ascii_alphabetic() || c == '_'
                    } else {
                        c.is_ascii_alphanumeric() || c == '_'
                    };
                    if !valid {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name.is_empty() {
                    return Err(syntax());
                }
            }
            if name == "__key__" {
                return Err(Status::invalid_argument(
                    "Invalid property path member name \"__key__\".",
                ));
            }
            let mut offsets = Vec::new();
            while chars.peek() == Some(&'[') {
                chars.next();
                let mut digits = String::new();
                while let Some(&c) = chars.peek() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    digits.push(c);
                    chars.next();
                }
                if digits.is_empty() || chars.next() != Some(']') {
                    return Err(syntax());
                }
                let significant = digits.trim_start_matches('0');
                let offset = match significant.len() {
                    0 => 0,
                    1..=10 => significant.parse::<u64>().expect("ten digits at most"),
                    _ => u64::MAX,
                };
                if offset > MAX_OFFSET {
                    return Err(Status::invalid_argument(format!(
                        "Invalid property path \"{path}\". Array offsets must be within the \
                         range [0, 2^31 - 1]."
                    )));
                }
                offsets.push(u32::try_from(offset).expect("checked above"));
            }
            segments.push(Segment { name, offsets });
            match chars.next() {
                None => return Ok(Self(segments)),
                Some('.') => {}
                Some(_) => return Err(syntax()),
            }
        }
    }

    /// The path is `__name__` alone: the document's name.
    /// The segments, with their offsets.
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    pub fn is_name(&self) -> bool {
        matches!(self.0.as_slice(), [only] if only.is_key() && only.offsets.is_empty())
    }

    pub fn has_offsets(&self) -> bool {
        self.0.iter().any(|s| !s.offsets.is_empty())
    }

    /// The path of a read or update mask, or of a projection: `__name__` alone (the name), no
    /// `__type__`, no array offsets, segment by segment.
    pub fn check_mask(&self) -> Result<(), Status> {
        for segment in &self.0 {
            if segment.is_key() && !self.is_name() {
                return Err(Status::invalid_argument("the name __key__ is reserved"));
            }
            if segment.name == "__type__" {
                return Err(Status::invalid_argument("the name __type__ is reserved"));
            }
            if !segment.offsets.is_empty() {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// The path of a filter, an order or an aggregation: `__name__` alone (the key), `__type__`
    /// only as the last segment of a longer path, no array offsets, segment by segment.
    pub fn check_query(&self) -> Result<(), Status> {
        let last = self.0.len() - 1;
        for (i, segment) in self.0.iter().enumerate() {
            if segment.is_key() && !self.is_name() {
                return Err(Status::invalid_argument("the name __key__ is reserved"));
            }
            if segment.name == "__type__" && !(i == last && i > 0 && segment.offsets.is_empty()) {
                return Err(Status::invalid_argument("the name __type__ is reserved"));
            }
            if !segment.offsets.is_empty() {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// The path `__type__` alone.
    pub fn is_type(&self) -> bool {
        matches!(self.0.as_slice(), [only] if only.name == "__type__" && only.offsets.is_empty())
    }

    /// A field path read and checked before, as a path of segments without array offsets.
    pub fn of(path: &FieldPath) -> Self {
        Self(
            path.segments()
                .iter()
                .map(|name| Segment {
                    name: name.clone(),
                    offsets: Vec::new(),
                })
                .collect(),
        )
    }

    /// The path of a field transform: no array offsets (`__name__` is the field `__key__`,
    /// `__type__` a field like another).
    pub fn check_transform(&self) -> Result<(), Status> {
        if self.has_offsets() {
            return Err(Status::invalid_argument(
                "Field transformation paths currently support only \".\".",
            ));
        }
        Ok(())
    }

    /// Whether `self` is a strict prefix of `other`, array offsets included.
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        let (a, b) = (self.elements(), other.elements());
        a.len() < b.len() && b.starts_with(&a)
    }

    fn elements(&self) -> Vec<Element<'_>> {
        self.0
            .iter()
            .flat_map(|s| {
                std::iter::once(Element::Name(s.shown()))
                    .chain(s.offsets.iter().map(|&i| Element::Offset(i)))
            })
            .collect()
    }

    /// The field path of a path without array offsets, its names as written. A segment with
    /// offsets becomes a name that holds them, which no field path reaches.
    pub fn field_path(&self) -> FieldPath {
        FieldPath::from_segments(self.0.iter().map(|s| {
            if s.offsets.is_empty() {
                s.name.clone()
            } else {
                Shown(std::slice::from_ref(s)).to_string()
            }
        }))
    }

    /// The field path a transform reads and writes: `__name__` is the field `__key__`.
    pub fn transform_field_path(&self) -> FieldPath {
        FieldPath::from_segments(self.0.iter().map(|s| s.shown().to_owned()))
    }
}

#[derive(PartialEq, Eq)]
enum Element<'a> {
    Name(&'a str),
    Offset(u32),
}

/// The path as the official emulator's messages print it: names unquoted, `__name__` as
/// `__key__`, array offsets as `[n]`, joined by `.`.
impl fmt::Display for PropertyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Shown(&self.0).fmt(f)
    }
}

struct Shown<'a>(&'a [Segment]);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(segment.shown())?;
            for offset in &segment.offsets {
                write!(f, "[{offset}]")?;
            }
        }
        Ok(())
    }
}

/// A field path printed as the official emulator's messages print one (see [`PropertyPath`]'s
/// `Display`).
pub fn shown(path: &FieldPath) -> String {
    PropertyPath::of(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(path: &str) -> String {
        PropertyPath::read(path).unwrap_err().message().to_owned()
    }

    #[test]
    fn reads_like_the_official_emulator() {
        assert_eq!(
            message("__key__.a b"),
            "Invalid property path member name \"__key__\"."
        );
        assert!(
            message("a b.__key__").starts_with("Invalid property path \"a b.__key__\". Unquoted")
        );
        assert_eq!(
            message("a[0].__x__.b[01]"),
            "Invalid reserved name in field path __x__.b[1]"
        );
        assert_eq!(
            message("__x__.__name__"),
            "Invalid reserved name in field path __x__.__key__"
        );
        assert_eq!(
            message("`a`.__x__.`b c`"),
            "Invalid reserved name in field path __x__.b c"
        );
        assert_eq!(
            message("a[2147483648].__key__"),
            "Invalid property path \"a[2147483648].__key__\". Array offsets must be within the \
             range [0, 2^31 - 1]."
        );
        assert!(PropertyPath::read("a[2147483647]").is_ok());
        assert!(PropertyPath::read("a[99999999999999999999999]").is_err());
        for path in ["a[", "a[]", "a[x]", "a[0]b", "[0]", "a.[0]", "a[-1]", "``"] {
            assert!(
                message(path).ends_with("(`(?:[^`\\\\]|(?:\\\\.))+`)"),
                "{path}"
            );
        }
    }

    #[test]
    fn checks_segments_by_context() {
        let check = |path: &str, f: fn(&PropertyPath) -> Result<(), Status>| {
            f(&PropertyPath::read(path).unwrap()).map_err(|s| s.message().to_owned())
        };
        assert_eq!(
            check("__type__.a[0]", PropertyPath::check_mask).unwrap_err(),
            "the name __type__ is reserved"
        );
        assert_eq!(
            check("a[0].__type__", PropertyPath::check_mask).unwrap_err(),
            "Invalid property path"
        );
        assert_eq!(
            check("__name__[0]", PropertyPath::check_mask).unwrap_err(),
            "the name __key__ is reserved"
        );
        assert!(check("__name__", PropertyPath::check_mask).is_ok());
        assert!(check("`__name__`", PropertyPath::check_mask).is_ok());
        assert!(check("a.__type__", PropertyPath::check_query).is_ok());
        assert_eq!(
            check("a.__type__[0]", PropertyPath::check_query).unwrap_err(),
            "the name __type__ is reserved"
        );
        assert_eq!(
            check("a.__type__.b", PropertyPath::check_query).unwrap_err(),
            "the name __type__ is reserved"
        );
        assert!(check("__type__.__name__", PropertyPath::check_transform).is_ok());
        assert!(check("a[0]", PropertyPath::check_transform).is_err());
        let a = PropertyPath::read("a").unwrap();
        assert!(a.is_prefix_of(&PropertyPath::read("a[0]").unwrap()));
        assert!(!a.is_prefix_of(&PropertyPath::read("a").unwrap()));
    }
}
