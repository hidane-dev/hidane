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
    ast::{Body, Callee, Expr, ExprKind, Function, PathPart, Ruleset, Span},
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

/// An expression of the report, with the expressions in it.
pub struct Report<'a> {
    pub position: Position,
    /// Each distinct outcome in the order first seen, and how many times it was seen.
    pub values: &'a [(Outcome, u64)],
    pub children: Vec<Report<'a>>,
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
                roots.push(self.expr(ruleset, condition));
            }
        }
    }

    fn function(&mut self, ruleset: &Ruleset, f: &Function) -> Option<usize> {
        if f.lets.is_empty() {
            return is_node(&f.result).then(|| self.expr(ruleset, &f.result));
        }
        Some(self.lets(ruleset, f, 0))
    }

    /// The body of `f` from its `i`th `let` on.
    fn lets(&mut self, ruleset: &Ruleset, f: &Function, i: usize) -> usize {
        let binding = &f.lets[i];
        let mut children = Vec::new();
        if is_node(&binding.value) {
            children.push(self.expr(ruleset, &binding.value));
        }
        if i + 1 < f.lets.len() {
            children.push(self.lets(ruleset, f, i + 1));
        } else if is_node(&f.result) {
            children.push(self.expr(ruleset, &f.result));
        }
        let position = self.position(binding.span, end(&f.result));
        self.add(NodeKey::Body(binding.span.start), position, children)
    }

    fn expr(&mut self, ruleset: &Ruleset, e: &Expr) -> usize {
        let children = parts(e)
            .into_iter()
            .filter(|part| is_node(part))
            .map(|part| self.expr(ruleset, part))
            .collect();
        let position = self.position(start(&ruleset.parens, e), end(e));
        self.add(NodeKey::of(e), position, children)
    }

    fn add(&mut self, key: NodeKey, position: Position, children: Vec<usize>) -> usize {
        self.nodes.push(Node {
            position,
            children,
            values: Vec::new(),
            seen: HashMap::new(),
        });
        let i = self.nodes.len() - 1;
        self.index.insert(key, i);
        i
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

    /// The report: its roots in order, each with the expressions in it.
    pub fn report(&self) -> Vec<Report<'_>> {
        self.roots.iter().map(|&i| self.node(i)).collect()
    }

    fn node(&self, i: usize) -> Report<'_> {
        let node = &self.nodes[i];
        Report {
            position: node.position,
            values: &node.values,
            children: node.children.iter().map(|&c| self.node(c)).collect(),
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

/// The expressions directly in `e`, in source order.
fn parts(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::List(items) => items.iter().collect(),
        ExprKind::Map(entries) => entries.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::Path(parts) => parts
            .iter()
            .filter_map(|part| match part {
                PathPart::Expr(e) => Some(e),
                PathPart::Segment(_) => None,
            })
            .collect(),
        ExprKind::Member { object, .. } => vec![object],
        ExprKind::Index { object, index } => vec![object, index],
        ExprKind::Slice { object, start, end } => vec![object, start, end],
        ExprKind::Call { callee, args } => match callee {
            Callee::Method { receiver, .. } => std::iter::once(&**receiver).chain(args).collect(),
            Callee::Global(_) | Callee::Namespaced { .. } => args.iter().collect(),
        },
        ExprKind::Unary { operand, .. } => vec![operand],
        ExprKind::Binary { left, right, .. } => vec![left, right],
        ExprKind::Is { value, .. } => vec![value],
        ExprKind::Ternary {
            condition,
            then,
            otherwise,
            ..
        } => vec![condition, then, otherwise],
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
            stack.extend(parts(e));
        }
    }
}

/// The first token of `e` in the report: the outermost parenthesis around it or around the
/// expression it starts with, or its own first token.
pub(crate) fn start(parens: &HashMap<(usize, usize), Span>, mut e: &Expr) -> Span {
    loop {
        if let Some(paren) = parens.get(&(e.span.start, e.span.end)) {
            return *paren;
        }
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
