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

use crate::ast::{Match, SegmentKind, Service, Version};

/// The value of a path variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// A wildcard's segment.
    Segment(String),
    /// A recursive wildcard's segments.
    Path(Vec<String>),
}

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
    path: &[&str],
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
    path: &[&str],
    at: usize,
) -> Vec<(usize, Vec<(String, Binding)>)> {
    let mut out = Vec::new();
    take(version, &m.path, path, at, &mut Vec::new(), &mut out);
    out
}

fn take(
    version: Version,
    segments: &[crate::ast::Segment],
    path: &[&str],
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
            if path.get(at) == Some(&literal.as_str()) {
                take(version, rest, path, at + 1, bound, out);
            }
        }
        SegmentKind::Wildcard(name) => {
            if let Some(segment) = path.get(at) {
                bound.push((name.clone(), Binding::Segment((*segment).to_owned())));
                take(version, rest, path, at + 1, bound, out);
                bound.pop();
            }
        }
        SegmentKind::Recursive(name) => {
            let fewest = match version {
                Version::V1 => 1,
                Version::V2 => 0,
            };
            for end in at + fewest..=path.len() {
                let segments = path[at..end].iter().map(|s| (*s).to_owned()).collect();
                bound.push((name.clone(), Binding::Path(segments)));
                take(version, rest, path, end, bound, out);
                bound.pop();
            }
        }
    }
}
