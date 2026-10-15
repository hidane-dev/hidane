//! Which `match` statements cover a path, and what their wildcards hold (#59).
//!
//! As measured with `tools/oracle/rules_paths.py` (`tests/fixtures/rules_paths.json`):
//!
//! - A `match` covers a path when its segments, after those of every `match` around it
//!   (outermost first, from the service), take the whole path: `databases`, the database ID,
//!   `documents`, then the document's segments. Its `allow` statements apply to that path only;
//!   a `match` that takes part of it only leads to the matches inside.
//! - A literal segment takes itself; a wildcard (`{d}`, `{d=*}`) takes one segment and holds it
//!   as a string.
//! - A recursive wildcard (`{rest=**}`) holds a path: one or more segments in rules version 1,
//!   where the compiler only allows it last, and zero or more in version 2, where it may come
//!   anywhere, once per `match`. So in version 2 `/c/{d}/{rest=**}` covers `c/d` itself, and a
//!   nested `/{rest=**}` covers the document of the `match` around it.
//!
//! A list is checked on its collection's path followed by an unknown document ID, and a
//! collection group's path holds an unknown run of segments before its collection ID
//! (`tools/oracle/rules_list.py`): only a wildcard takes the unknown ID, and leaves its variable
//! unbound (using it is a `Null value error`); only a recursive wildcard takes an unknown run.

use crate::ast::{Match, SegmentKind, Service, Version};

/// The value of a path variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// A wildcard's segment.
    Segment(String),
    /// A recursive wildcard's segments; an unknown part is [`UNKNOWN_ID`] or [`UNKNOWN_RUN`].
    Path(Vec<String>),
    /// A wildcard that took an unknown document or collection ID.
    Unbound,
}

/// A segment of a path to cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part<'p> {
    Segment(&'p str),
    /// The document ID of a list, or the collection ID of one without (`from` with an empty
    /// collection ID): any one segment.
    UnknownId,
    /// The parents of a collection group's collections: any number of segments.
    UnknownRun,
}

/// How an unknown document ID shows in a recursive wildcard's path (no segment holds it).
pub const UNKNOWN_ID: &str = "\u{0}*";
/// How an unknown run of segments shows in a recursive wildcard's path.
pub const UNKNOWN_RUN: &str = "\u{0}**";

/// A `match` that covers a path: the matches from the service down to it, and the variables
/// they bound, outermost first.
#[derive(Debug, Clone)]
pub struct Covering<'a> {
    pub chain: Vec<&'a Match>,
    pub bindings: Vec<(String, Binding)>,
}

impl Covering<'_> {
    /// The innermost `match`, whose `allow` statements apply.
    pub fn target(&self) -> &Match {
        self.chain
            .last()
            .expect("a covering has at least one match")
    }
}

/// Every `match` of `service` that covers `path`, in source order (a `match` before the ones
/// inside it). A `match` that covers it in several ways (two recursive wildcards splitting it
/// differently) is listed once, with the first way found, which gives earlier recursive
/// wildcards fewer segments.
pub fn covering<'a>(version: Version, service: &'a Service, path: &[&str]) -> Vec<Covering<'a>> {
    let parts: Vec<Part<'_>> = path.iter().map(|s| Part::Segment(s)).collect();
    covering_parts(version, service, &parts)
}

/// [`covering`] for a path with unknown parts.
pub fn covering_parts<'a>(
    version: Version,
    service: &'a Service,
    path: &[Part<'_>],
) -> Vec<Covering<'a>> {
    let mut found = Vec::new();
    for m in &service.body.matches {
        visit(
            version,
            m,
            path,
            0,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut found,
        );
    }
    found
}

fn visit<'a>(
    version: Version,
    m: &'a Match,
    path: &[Part<'_>],
    at: usize,
    chain: &mut Vec<&'a Match>,
    bindings: &mut Vec<(String, Binding)>,
    found: &mut Vec<Covering<'a>>,
) {
    chain.push(m);
    let mut covered = false;
    for (end, taken) in prefixes(version, m, path, at) {
        let depth = bindings.len();
        bindings.extend(taken);
        if end == path.len() && !covered {
            covered = true;
            found.push(Covering {
                chain: chain.clone(),
                bindings: bindings.clone(),
            });
        }
        for inner in &m.body.matches {
            visit(version, inner, path, end, chain, bindings, found);
        }
        bindings.truncate(depth);
    }
    chain.pop();
}

/// Every way the segments of `m` can take a prefix of `path[at..]`: where it ends, and what
/// its variables hold.
fn prefixes(
    version: Version,
    m: &Match,
    path: &[Part<'_>],
    at: usize,
) -> Vec<(usize, Vec<(String, Binding)>)> {
    let mut out = Vec::new();
    take(version, &m.path, path, at, &mut Vec::new(), &mut out);
    out
}

fn take(
    version: Version,
    segments: &[crate::ast::Segment],
    path: &[Part<'_>],
    at: usize,
    bound: &mut Vec<(String, Binding)>,
    out: &mut Vec<(usize, Vec<(String, Binding)>)>,
) {
    let Some((first, rest)) = segments.split_first() else {
        out.push((at, bound.clone()));
        return;
    };
    match &first.kind {
        SegmentKind::Literal(literal) => {
            if path.get(at) == Some(&Part::Segment(literal)) {
                take(version, rest, path, at + 1, bound, out);
            }
        }
        SegmentKind::Wildcard(name) => {
            let binding = match path.get(at) {
                Some(Part::Segment(segment)) => Binding::Segment((*segment).to_owned()),
                Some(Part::UnknownId) => Binding::Unbound,
                Some(Part::UnknownRun) | None => return,
            };
            bound.push((name.clone(), binding));
            take(version, rest, path, at + 1, bound, out);
            bound.pop();
        }
        SegmentKind::Recursive(name) => {
            let fewest = match version {
                Version::V1 => 1,
                Version::V2 => 0,
            };
            for end in at + fewest..=path.len() {
                let segments = path[at..end]
                    .iter()
                    .map(|part| match part {
                        Part::Segment(s) => (*s).to_owned(),
                        Part::UnknownId => UNKNOWN_ID.to_owned(),
                        Part::UnknownRun => UNKNOWN_RUN.to_owned(),
                    })
                    .collect();
                bound.push((name.clone(), Binding::Path(segments)));
                take(version, rest, path, end, bound, out);
                bound.pop();
            }
        }
    }
}
