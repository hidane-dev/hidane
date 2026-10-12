//! The coverage report of `GET /emulator/v1/projects/{p}:ruleCoverage`: what each expression
//! of a ruleset evaluated to, as the official emulator reports it
//! (`tools/oracle/rules_coverage.py`, `crates/hidane/tests/fixtures/rules_coverage.json`).
//!
//! - The report holds every expression of the ruleset's first file that is not a literal (null,
//!   a bool, a number, a string or bytes), each with the ones in it. Its roots are the
//!   functions' bodies and the `allow` conditions in the order the ruleset nests them: the
//!   file's functions, then each service's functions, its `match` statements (each with its
//!   functions, the `match` statements in it, then its `allow` statements) and its `allow`
//!   statements. A body or a condition that is a literal is left out.
//! - A function body with `let`s starts at its first `let`: each `let` holds its value and the
//!   body from the next `let` on (or the result), and has the function's result.
//! - An expression starts at its first token, or at the outermost parenthesis before it, and
//!   ends with its last part: a call with its last argument (or its name), a list or a map with
//!   its last item (or its opening bracket), a path with its last segment, a member with its
//!   name; closing brackets are left out. Offsets count code points, and the end is inclusive.
//! - Each expression lists the distinct things it evaluated to, in the order first seen, with
//!   how many times: a value, an error, or nothing (`{}`) when `&&`, `||` or `?:` decided
//!   without it. An error is placed at a token of the expression that raised it (an operator,
//!   a function's name, or the expression's first token) and keeps that place as it goes up.
//!
//! The official emulator evaluates a condition again after an error, and its report counts
//! both passes; hidane's counts each evaluation once (`docs/parity-exceptions.md`). It also
//! keeps at most [`MAX_VALUES`] distinct values per expression.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use crate::{
    ast::{BinaryOp, Body, Callee, Expr, ExprKind, Function, PathPart, Ruleset, Span},
    diag::Position,
    eval::{NodeKey, Outcome, Trace, Traced, observe},
    lexer,
};

/// How many distinct values an expression keeps; later ones that differ from all of them are
/// not counted.
pub const MAX_VALUES: usize = 100;

/// A ruleset's report: its expressions and what each evaluated to so far.
pub struct Coverage {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    index: HashMap<NodeKey, usize>,
    /// The code points before each byte of the file.
    code_points: Vec<u32>,
    /// Each token's end, line and column, by its start (bytes), to place errors.
    tokens: HashMap<usize, (usize, u32, u32)>,
}

struct Node {
    position: Position,
    children: Vec<usize>,
    values: Vec<(Outcome, u64)>,
    /// Indexes into `values`, by the hash of the outcome.
    seen: HashMap<u64, Vec<usize>>,
}

/// An expression of the report.
pub struct Expression<'a> {
    pub position: Position,
    /// Each distinct outcome in the order first seen, and how many times it was seen.
    pub values: &'a [(Outcome, u64)],
    /// The expressions in it, in order ([`Coverage::expression`]).
    pub children: &'a [usize],
}

/// What the report still has to add: an expression (and whether it is the object of a member,
/// an index or a method, whose report leaves out its outermost parenthesis), or a function's
/// body from its `i`th `let` on.
enum Pending<'r> {
    Expr(&'r Expr, bool),
    Lets(&'r Function, usize),
}

impl Coverage {
    /// The report of `ruleset`, compiled from `source` (its first file), with nothing counted.
    pub fn new(ruleset: &Ruleset, source: &str) -> Self {
        let mut code_points = Vec::with_capacity(source.len() + 1);
        let mut count = 0u32;
        for c in source.chars() {
            code_points.extend(std::iter::repeat_n(count, c.len_utf8()));
            count += 1;
        }
        code_points.push(count);
        let tokens = lexer::lex(source)
            .tokens
            .into_iter()
            .map(|t| (t.start, (t.end, t.line, t.column)))
            .collect();
        let mut coverage = Self {
            nodes: Vec::new(),
            roots: Vec::new(),
            index: HashMap::new(),
            code_points,
            tokens,
        };
        let mut roots = Vec::new();
        for f in &ruleset.functions {
            roots.extend(coverage.function(ruleset, f));
        }
        for service in &ruleset.services {
            coverage.body(ruleset, &service.body, &mut roots);
        }
        coverage.roots = roots;
        coverage
    }

    fn body(&mut self, ruleset: &Ruleset, body: &Body, roots: &mut Vec<usize>) {
        for f in &body.functions {
            roots.extend(self.function(ruleset, f));
        }
        for m in &body.matches {
            self.body(ruleset, &m.body, roots);
        }
        for allow in &body.allows {
            if let Some(condition) = allow.condition.as_ref().filter(|c| is_node(c)) {
                roots.push(self.tree(ruleset, Pending::Expr(condition, false)));
            }
        }
    }

    fn function(&mut self, ruleset: &Ruleset, f: &Function) -> Option<usize> {
        if f.lets.is_empty() {
            return is_node(&f.result).then(|| self.tree(ruleset, Pending::Expr(&f.result, false)));
        }
        Some(self.tree(ruleset, Pending::Lets(f, 0)))
    }

    /// Adds `root` and everything in it (without recursion: expressions nest thousands deep).
    fn tree(&mut self, ruleset: &Ruleset, root: Pending<'_>) -> usize {
        let mut stack: Vec<(Pending<'_>, Option<usize>)> = vec![(root, None)];
        let mut added = None;
        while let Some((pending, parent)) = stack.pop() {
            let mut children = Vec::new();
            let (key, position) = match pending {
                Pending::Expr(e, object) => {
                    children.extend(
                        parts(e)
                            .into_iter()
                            .filter(|(part, _)| is_node(part))
                            .map(|(part, object)| Pending::Expr(part, object)),
                    );
                    let start = if object {
                        start_inside(&ruleset.parens, e)
                    } else {
                        start(&ruleset.parens, e)
                    };
                    (NodeKey::of(e), self.position(start, end(e)))
                }
                Pending::Lets(f, i) => {
                    let binding = &f.lets[i];
                    if is_node(&binding.value) {
                        children.push(Pending::Expr(&binding.value, false));
                    }
                    if i + 1 < f.lets.len() {
                        children.push(Pending::Lets(f, i + 1));
                    } else if is_node(&f.result) {
                        children.push(Pending::Expr(&f.result, false));
                    }
                    let position = self.position(binding.span, end(&f.result));
                    (NodeKey::Body(binding.span.start), position)
                }
            };
            self.nodes.push(Node {
                position,
                children: Vec::new(),
                values: Vec::new(),
                seen: HashMap::new(),
            });
            let index = self.nodes.len() - 1;
            self.index.insert(key, index);
            match parent {
                Some(parent) => self.nodes[parent].children.push(index),
                None => added = Some(index),
            }
            stack.extend(children.into_iter().rev().map(|c| (c, Some(index))));
        }
        added.expect("the root was added")
    }

    /// From the token of `start` to byte `end` (exclusive).
    fn position(&self, start: Span, end: usize) -> Position {
        let from = self.code_points[start.start];
        Position {
            line: start.line,
            column: start.column,
            current_offset: from,
            end_offset: self.code_points[end].max(from + 1) - 1,
        }
    }

    /// The token that starts at byte `at`.
    fn token(&self, at: usize) -> Option<Position> {
        let &(end, line, column) = self.tokens.get(&at)?;
        Some(self.position(
            Span {
                start: at,
                end,
                line,
                column,
            },
            end,
        ))
    }

    /// Counts what `trace` saw of this ruleset's expressions.
    pub fn record(&mut self, trace: &Trace) {
        let Some(values) = &trace.values else {
            return;
        };
        for (key, traced) in values {
            let Some(&i) = self.index.get(key) else {
                continue;
            };
            let outcome = match traced {
                Traced::Value(v) => Outcome::Value(observe(v)),
                Traced::Error(e) => Outcome::Error {
                    position: e.at.and_then(|at| self.token(at)),
                    message: e.message.clone(),
                },
                Traced::Skipped => Outcome::Skipped,
            };
            self.nodes[i].count(outcome);
        }
    }

    /// The report's expressions at the top, in order ([`Coverage::expression`]).
    pub fn roots(&self) -> &[usize] {
        &self.roots
    }

    /// The expression `i` of the report.
    pub fn expression(&self, i: usize) -> Expression<'_> {
        let node = &self.nodes[i];
        Expression {
            position: node.position,
            values: &node.values,
            children: &node.children,
        }
    }
}

impl Node {
    fn count(&mut self, outcome: Outcome) {
        let mut hasher = DefaultHasher::new();
        outcome.hash(&mut hasher);
        let hash = hasher.finish();
        if let Some(seen) = self.seen.get(&hash) {
            for &i in seen {
                if self.values[i].0 == outcome {
                    self.values[i].1 += 1;
                    return;
                }
            }
        }
        if self.values.len() < MAX_VALUES {
            self.seen.entry(hash).or_default().push(self.values.len());
            self.values.push((outcome, 1));
        }
    }
}

/// Whether `e` is in the report: every expression but a literal.
pub(crate) fn is_node(e: &Expr) -> bool {
    !matches!(
        e.kind,
        ExprKind::Null
            | ExprKind::Bool(_)
            | ExprKind::Int(_)
            | ExprKind::IntOverflow(_)
            | ExprKind::Float(_)
            | ExprKind::String(_)
            | ExprKind::Bytes(_)
    )
}

/// The expressions directly in `e`, in the official emulator's order, each with whether it is
/// the object of a member, an index or a method.
fn parts(e: &Expr) -> Vec<(&Expr, bool)> {
    match &e.kind {
        ExprKind::List(items) => items.iter().map(|e| (e, false)).collect(),
        ExprKind::Map(entries) => entries
            .iter()
            .flat_map(|(k, v)| [(k, false), (v, false)])
            .collect(),
        ExprKind::Path(parts) => parts
            .iter()
            .filter_map(|part| match part {
                PathPart::Expr(e) => Some((e, false)),
                PathPart::Segment(_) => None,
            })
            .collect(),
        ExprKind::Member { object, .. } => vec![(object, true)],
        ExprKind::Index { object, index } => vec![(object, true), (index, false)],
        ExprKind::Slice { object, start, end } => {
            vec![(object, true), (start, false), (end, false)]
        }
        ExprKind::Call { callee, args } => {
            let mut parts = Vec::with_capacity(args.len() + 1);
            if let Callee::Method { receiver, .. } = callee {
                parts.push((&**receiver, true));
            }
            parts.extend(args.iter().map(|a| (a, false)));
            parts
        }
        ExprKind::Unary { operand, .. } => vec![(operand, false)],
        // `x in y` is `y`'s method there.
        ExprKind::Binary {
            op: BinaryOp::In,
            left,
            right,
            ..
        } => vec![(right, false), (left, false)],
        ExprKind::Binary { left, right, .. } => vec![(left, false), (right, false)],
        ExprKind::Is { value, .. } => vec![(value, false)],
        ExprKind::Ternary {
            condition,
            then,
            otherwise,
            ..
        } => vec![(condition, false), (then, false), (otherwise, false)],
        ExprKind::Null
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::IntOverflow(_)
        | ExprKind::Float(_)
        | ExprKind::String(_)
        | ExprKind::Bytes(_)
        | ExprKind::Ident(_) => Vec::new(),
    }
}

/// Calls `f` on `e` and on every expression in it that is in the report.
pub(crate) fn walk<'e>(e: &'e Expr, f: &mut dyn FnMut(&'e Expr)) {
    let mut stack = vec![e];
    while let Some(e) = stack.pop() {
        if is_node(e) {
            f(e);
            stack.extend(parts(e).into_iter().map(|(part, _)| part));
        }
    }
}

/// The first token of `e` in the report: the outermost parenthesis around it or around the
/// expression it starts with, or its own first token.
pub(crate) fn start(parens: &HashMap<(usize, usize), Vec<Span>>, e: &Expr) -> Span {
    match parens.get(&(e.span.start, e.span.end)) {
        Some(around) if !around.is_empty() => around[0],
        _ => leftmost(parens, e),
    }
}

/// The first token of an expression that is the object of a member, an index or a method: its
/// outermost parenthesis is the outer expression's.
fn start_inside(parens: &HashMap<(usize, usize), Vec<Span>>, e: &Expr) -> Span {
    match parens.get(&(e.span.start, e.span.end)) {
        Some(around) if around.len() > 1 => around[1],
        _ => leftmost(parens, e),
    }
}

/// The first token of `e` but for parentheses around it: that of the expression it starts with.
fn leftmost(parens: &HashMap<(usize, usize), Vec<Span>>, mut e: &Expr) -> Span {
    loop {
        e = match &e.kind {
            ExprKind::Binary { left, .. } => left,
            ExprKind::Member { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::Slice { object, .. } => object,
            ExprKind::Call {
                callee: Callee::Method { receiver, .. },
                ..
            } => receiver,
            ExprKind::Is { value, .. } => value,
            ExprKind::Ternary { condition, .. } => condition,
            _ => return e.span,
        };
        if let Some(around) = parens.get(&(e.span.start, e.span.end))
            && let Some(outermost) = around.first()
        {
            return *outermost;
        }
    }
}

/// Where `e` ends in the report (bytes, exclusive): with its last part, closing brackets left
/// out.
fn end(mut e: &Expr) -> usize {
    loop {
        e = match &e.kind {
            ExprKind::List(items) => match items.last() {
                Some(last) => last,
                None => return e.span.start + 1,
            },
            ExprKind::Map(entries) => match entries.last() {
                Some((_, value)) => value,
                None => return e.span.start + 1,
            },
            ExprKind::Path(parts) => match parts.last() {
                Some(PathPart::Expr(last)) => last,
                _ => return e.span.end,
            },
            ExprKind::Member { field, .. } => return field.span.end,
            ExprKind::Index { index, .. } => index,
            ExprKind::Slice { end, .. } => end,
            ExprKind::Call { callee, args } => match args.last() {
                Some(last) => last,
                None => {
                    return match callee {
                        Callee::Global(name)
                        | Callee::Namespaced { name, .. }
                        | Callee::Method { name, .. } => name.span.end,
                    };
                }
            },
            ExprKind::Unary { operand, .. } => operand,
            ExprKind::Binary { right, .. } => right,
            ExprKind::Is { type_name, .. } => return type_name.span.end,
            ExprKind::Ternary { otherwise, .. } => otherwise,
            _ => return e.span.end,
        };
    }
}
