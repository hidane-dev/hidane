//! The checks after parsing, in the official compiler's stages:
//!
//! 1. [`structure`]: the version, imports and `match` paths (recursive wildcards, repeated
//!    variables). An unsupported version stops here.
//! 2. [`packages`]: a namespace (`math`, `duration`, …) used as a variable; [`complexity`]:
//!    expressions nested too deep, too many `let`s.
//! 3. [`semantics`], only when nothing so far was an error, and only for the first file:
//!    names (functions, variables, methods, services), arities, unused declarations, recursion,
//!    regular expression literals, `let` in version 1, the limits on parameters, call depth and
//!    `match` nesting, paths and variables, and the types.
//!
//! The types are checked as the official compiler checks them (measured with
//! `tools/oracle/rules_warnings.py`, `tests/fixtures/rules_warnings.json`), and every finding is
//! a warning:
//!
//! - Each expression has a set of types, from what it is: a literal's, an operator's or a
//!   function's results (`+` gives `int, float, string, duration, timestamp` whatever its
//!   operands), a wildcard's. A field, an index, `get()`, a conditional, `request` and a
//!   parameter or `let` have any type.
//! - Each operand, receiver and argument is expected to have some types: an operator's for the
//!   result its parent expects (`(x + y).lower()` expects strings), a function's parameter's,
//!   `bool` for a condition. An expression that has none of them is reported ("Invalid type.
//!   Received one of [...]. Expected one of [...]."), and what is in it is then expected to be
//!   anything, except a map's keys (strings) and a path's `$( )` parts.
//! - A conditional passes the expected types to both results and warns when they share no
//!   type. `==` and `!=` warn when their operands' types cannot be equal.
//! - A wildcard that cannot be of the expected type is reported with "There is no type for
//!   variable". A parameter or `let` takes the types its uses expect, use after use: from the
//!   result, then from the `let`s, last first. When none is left, that is reported at the
//!   statement, and it takes any type again. A `let`'s value is expected to be of its type.
//! - A call of one of the ruleset's functions expects each argument to be of the type the
//!   function's body requires of the parameter, and, where the function returns a parameter
//!   (directly, through `let`s, conditionals and calls), of the type expected of the call.

use std::{collections::HashMap, rc::Rc};

use crate::{
    ast::*,
    diag::Diag,
    parser::{FileAst, is_package},
};

/// The version a file declares, or the error for an unsupported one.
pub(crate) fn version(file: &FileAst, diags: &mut Vec<Diag>) -> Option<Version> {
    match file.version.as_ref() {
        None => Some(Version::V1),
        Some(v) if v.value == "1" => Some(Version::V1),
        Some(v) if v.value == "2" => Some(Version::V2),
        Some(v) => {
            diags.push(Diag::error(
                v.span,
                format!("Unsupported rules version {}.", v.value),
            ));
            None
        }
    }
}

/// Imports and `match` paths.
/// Imports and `match` paths. Returns whether `match`es nest too deep to go on.
pub(crate) fn structure(file: &FileAst, version: Option<Version>, diags: &mut Vec<Diag>) -> bool {
    for import in &file.imports {
        diags.push(Diag::error(
            import.span,
            "Import statements are not allowed.",
        ));
        diags.push(Diag::error(
            import.span,
            format!(
                "Module \"{}\" is not part of the standard library.",
                import.module
            ),
        ));
    }
    let version = version.unwrap_or(Version::V1);
    let mut too_deep = false;
    for service in &file.services {
        for m in &service.body.matches {
            too_deep |= paths(m, version, 0, 0, diags);
        }
    }
    too_deep
}

/// How deep `match`es may nest (the first of a service is at 0) before the compiler gives up.
const MAX_HIERARCHY: usize = 63;

/// Checks the path of `m` and of the `match`es in it; returns whether they nest too deep.
fn paths(
    m: &Match,
    version: Version,
    outer_globs: usize,
    depth: usize,
    diags: &mut Vec<Diag>,
) -> bool {
    if depth == MAX_HIERARCHY {
        diags.push(Diag::error(
            m.span,
            "Rule hierarchy is too complex to evaluate safely.",
        ));
        return true;
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut globs = 0;
    let last = m.path.len().saturating_sub(1);
    for (i, segment) in m.path.iter().enumerate() {
        if let SegmentKind::Recursive(_) = segment.kind {
            match version {
                Version::V1 if i != last => diags.push(Diag::error(
                    segment.span,
                    "Invalid glob match expression. Glob matches are permitted as the last \
                     segment in a match declaration path.",
                )),
                Version::V2 if outer_globs + globs > 0 => diags.push(Diag::error(
                    if globs > 0 { segment.span } else { m.span },
                    "Invalid glob match expression. Only one glob match is permitted in a \
                     match declaration path.",
                )),
                _ => {}
            }
            globs += 1;
        }
        if let Some(name) = segment.kind.variable() {
            if seen.contains(&name) {
                diags.push(Diag::error(
                    segment.name_span,
                    format!("Variable {name} already exists in the PathTemplate."),
                ));
            }
            seen.push(name);
        }
    }
    let mut too_deep = false;
    for child in &m.body.matches {
        too_deep |= paths(child, version, outer_globs + globs, depth + 1, diags);
    }
    too_deep
}

/// An int literal beyond 64 bits, except the smallest int written negated:
/// `-9223372036854775808` compiles, `-(9223372036854775808)` does not.
pub(crate) fn numbers(file: &FileAst, diags: &mut Vec<Diag>) {
    for_each_expression(file, &mut |root| {
        let mut stack = vec![(root, false)];
        while let Some((e, negated)) = stack.pop() {
            if let ExprKind::IntOverflow(text) = &e.kind {
                let bare = !file.parens.contains_key(&(e.span.start, e.span.end));
                if !(negated && bare && text == "9223372036854775808") {
                    diags.push(Diag::error(
                        e.span,
                        format!("Invalid number literal {text}."),
                    ));
                }
            }
            let negation = matches!(
                e.kind,
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    ..
                }
            );
            stack.extend(children(e).into_iter().map(|(child, _)| (child, negation)));
        }
    });
}

/// A namespace used as a variable: `math == 1`, `math.PI`.
pub(crate) fn packages(file: &FileAst, diags: &mut Vec<Diag>) {
    let mut visit = |e: &Expr| {
        walk(e, &mut |e| {
            if let ExprKind::Ident(name) = &e.kind
                && is_package(name)
            {
                diags.push(Diag::error(
                    e.span,
                    format!("{name} is a package and cannot be used as variable name."),
                ));
            }
        });
    };
    for_each_expression(file, &mut visit);
}

/// Calls `f` on every expression of the file, outermost first.
fn for_each_expression(file: &FileAst, f: &mut impl FnMut(&Expr)) {
    fn function(fun: &Function, f: &mut impl FnMut(&Expr)) {
        for l in &fun.lets {
            f(&l.value);
        }
        f(&fun.result);
    }
    fn body(b: &Body, f: &mut impl FnMut(&Expr)) {
        for fun in &b.functions {
            function(fun, f);
        }
        for a in &b.allows {
            if let Some(c) = &a.condition {
                f(c);
            }
        }
        for m in &b.matches {
            body(&m.body, f);
        }
    }
    for fun in &file.functions {
        function(fun, f);
    }
    for s in &file.services {
        body(&s.body, f);
    }
}

/// Calls `f` on `e` and every expression in it, outermost first. Without recursion: a chain of
/// operators can be thousands deep.
fn walk<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Expr)) {
    let mut stack = vec![e];
    while let Some(e) = stack.pop() {
        f(e);
        stack.extend(children(e).into_iter().rev().map(|(child, _)| child));
    }
}

/// The expressions directly in `e`, each with whether the compiler counts it one level deeper
/// (accessed values and method receivers are not).
fn children(e: &Expr) -> Vec<(&Expr, bool)> {
    match &e.kind {
        ExprKind::List(items) => items.iter().map(|i| (i, true)).collect(),
        ExprKind::Map(entries) => entries
            .iter()
            .flat_map(|(k, v)| [(k, true), (v, true)])
            .collect(),
        ExprKind::Path(parts) => parts
            .iter()
            .filter_map(|part| match part {
                PathPart::Expr(e) => Some((e, true)),
                PathPart::Segment(_) => None,
            })
            .collect(),
        ExprKind::Member { object, .. } => vec![(object, false)],
        ExprKind::Index { object, index } => vec![(object, false), (index, true)],
        ExprKind::Slice { object, start, end } => {
            vec![(object, false), (start, true), (end, true)]
        }
        ExprKind::Call { callee, args } => {
            let mut children = Vec::with_capacity(args.len() + 1);
            if let Callee::Method { receiver, .. } = callee {
                children.push((&**receiver, false));
            }
            children.extend(args.iter().map(|a| (a, true)));
            children
        }
        ExprKind::Unary { operand, .. } => vec![(operand, true)],
        ExprKind::Binary { left, right, .. } => vec![(left, true), (right, true)],
        ExprKind::Is { value, .. } => vec![(value, true)],
        ExprKind::Ternary {
            condition,
            then,
            otherwise,
            ..
        } => vec![(condition, true), (then, true), (otherwise, true)],
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

/// The depth at which the compiler finds an expression too complex, counting from 0 at a
/// statement's expression: every level of brackets, operators, calls, lists and maps counts.
const COMPLEX_DEPTH: usize = 99;
/// The deepest an expression may be counting every node, member accesses included (the
/// compiler sets no such limit and fails far deeper).
const MAX_TREE_DEPTH: usize = 2_000;
/// The most `let`s a function may have.
const MAX_LETS: usize = 11;
const TOO_COMPLEX: &str = "Expression is too complex to evaluate safely.";

/// "Expression is too complex to evaluate safely." at each expression (or parenthesis) at the
/// compiler's depth limit, and the limit on `let`s.
pub(crate) fn complexity(file: &FileAst, diags: &mut Vec<Diag>) {
    for_each_expression(file, &mut |root| depth(root, file, diags));
    fn lets(f: &Function, diags: &mut Vec<Diag>) {
        if f.lets.len() > MAX_LETS {
            diags.push(Diag::error(
                f.result.span,
                "Maximum allowed variable count of 10 for a given function has been reached.",
            ));
        }
    }
    fn body(b: &Body, diags: &mut Vec<Diag>) {
        b.functions.iter().for_each(|f| lets(f, diags));
        b.matches.iter().for_each(|m| body(&m.body, diags));
    }
    file.functions.iter().for_each(|f| lets(f, diags));
    file.services.iter().for_each(|s| body(&s.body, diags));
}

fn depth(root: &Expr, file: &FileAst, diags: &mut Vec<Diag>) {
    // (expression, its depth outside its parentheses, how many nodes are above it, whether
    // it is negated: `-(x)` is one level, not two)
    let mut stack = vec![(root, 0, 0, false)];
    let mut too_deep = false;
    while let Some((e, mut level, nodes, negated)) = stack.pop() {
        let parens = file.parens.get(&(e.span.start, e.span.end));
        for paren in parens.into_iter().flatten().skip(usize::from(negated)) {
            if level == COMPLEX_DEPTH {
                diags.push(Diag::error(*paren, TOO_COMPLEX));
            }
            level += 1;
        }
        if level == COMPLEX_DEPTH {
            diags.push(Diag::error(e.span, TOO_COMPLEX));
        }
        // Past the compiler's depth, the error is reported already.
        if nodes > MAX_TREE_DEPTH && level < COMPLEX_DEPTH {
            if !too_deep {
                diags.push(Diag::error(e.span, TOO_COMPLEX));
                too_deep = true;
            }
            continue;
        }
        let negation = matches!(
            e.kind,
            ExprKind::Unary {
                op: UnaryOp::Neg,
                ..
            }
        );
        for (child, counts) in children(e).into_iter().rev() {
            stack.push((child, level + usize::from(counts), nodes + 1, negation));
        }
    }
}

// ----- stage 3

/// The services the emulator knows, and the methods their rules may name.
const SERVICES: &[&str] = &["cloud.firestore", "firebase.storage"];

/// A type of the official type checker, as its messages name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum T {
    Int,
    Float,
    String,
    Bytes,
    Bool,
    Null,
    List,
    Map,
    Set,
    MapDiff,
    Path,
    Timestamp,
    Duration,
    LatLng,
    Vector,
    Constraint,
}

impl T {
    fn name(self) -> &'static str {
        match self {
            T::Int => "int",
            T::Float => "float",
            T::String => "string",
            T::Bytes => "bytes",
            T::Bool => "bool",
            T::Null => "null",
            T::List => "list",
            T::Map => "map",
            T::Set => "set",
            T::MapDiff => "map_diff",
            T::Path => "path",
            T::Timestamp => "timestamp",
            T::Duration => "duration",
            T::LatLng => "latlng",
            T::Vector => "vector",
            T::Constraint => "constraint",
        }
    }
}

/// Some types, in the order the messages list them (each set has its own order).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Types {
    len: u8,
    items: [T; 16],
}

impl Types {
    const fn of(types: &[T]) -> Self {
        let mut items = [T::Int; 16];
        let mut i = 0;
        while i < types.len() {
            items[i] = types[i];
            i += 1;
        }
        Self {
            len: types.len() as u8,
            items,
        }
    }

    fn as_slice(&self) -> &[T] {
        &self.items[..usize::from(self.len)]
    }

    fn contains(&self, t: T) -> bool {
        self.as_slice().contains(&t)
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, t: T) {
        if !self.contains(t) {
            self.items[usize::from(self.len)] = t;
            self.len += 1;
        }
    }

    /// The types of `self` that are in `other`, in `self`'s order.
    fn filter(self, other: Types) -> Types {
        let mut out = Types::of(&[]);
        for &t in self.as_slice() {
            if other.contains(t) {
                out.push(t);
            }
        }
        out
    }

    fn meets(self, other: Types) -> bool {
        self.as_slice().iter().any(|&t| other.contains(t))
    }

    /// Whether a value of one could equal a value of the other: they share a type, or both
    /// have a number.
    fn comparable(self, other: Types) -> bool {
        let number = |t: &T| matches!(t, T::Int | T::Float);
        self.meets(other)
            || (self.as_slice().iter().any(number) && other.as_slice().iter().any(number))
    }

    /// `int, float`.
    fn list(&self) -> String {
        self.as_slice()
            .iter()
            .map(|t| t.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A type expected or found: some types, or any (`None`): `request`, a field, a parameter.
type Ty = Option<Types>;

/// A function and the type expected of its result, for which its signature is worked out.
type Call = (usize, Ty);

const ANY: Ty = None;
const BOOL: Types = Types::of(&[T::Bool]);
const INT: Types = Types::of(&[T::Int]);
const FLOAT: Types = Types::of(&[T::Float]);
const NUMBER: Types = Types::of(&[T::Int, T::Float]);
const FLOAT_INT: Types = Types::of(&[T::Float, T::Int]);
const STRING: Types = Types::of(&[T::String]);
const BYTES: Types = Types::of(&[T::Bytes]);
const LIST: Types = Types::of(&[T::List]);
const MAP: Types = Types::of(&[T::Map]);
const SET: Types = Types::of(&[T::Set]);
const PATH: Types = Types::of(&[T::Path]);
const DURATION: Types = Types::of(&[T::Duration]);
const TIMESTAMP: Types = Types::of(&[T::Timestamp]);
const LATLNG: Types = Types::of(&[T::LatLng]);
const MAP_DIFF: Types = Types::of(&[T::MapDiff]);
const TIME: Types = Types::of(&[T::Duration, T::Timestamp]);
const LIST_SET: Types = Types::of(&[T::List, T::Set]);
const SIZED: Types = Types::of(&[T::String, T::Bytes, T::List, T::Set, T::Map]);
const INDEXED: Types = Types::of(&[T::String, T::List, T::Map, T::Path]);
const SLICED: Types = Types::of(&[T::String, T::List, T::Path]);
const KEY: Types = Types::of(&[T::Int, T::String]);
const BOUND: Types = Types::of(&[T::Null, T::Int]);
const CONTAINER: Types = Types::of(&[T::List, T::Set, T::Map, T::Constraint]);
const PATH_PART: Types = Types::of(&[T::Path, T::Null, T::Int, T::Float, T::String, T::Bool]);

/// A built-in function or method: its parameters (each any or some types) and result. A
/// method also has the types of its receiver.
struct Builtin {
    name: &'static str,
    receiver: Types,
    params: &'static [Ty],
    result: Ty,
}

const fn global(name: &'static str, params: &'static [Ty], result: Ty) -> Builtin {
    Builtin {
        name,
        receiver: Types::of(&[]),
        params,
        result,
    }
}

const fn method(name: &'static str, receiver: Types, params: &'static [Ty], result: Ty) -> Builtin {
    Builtin {
        name,
        receiver,
        params,
        result,
    }
}

const fn of(types: Types) -> Ty {
    Some(types)
}

/// Global functions, as measured.
const GLOBALS: &[Builtin] = &[
    global("get", &[of(PATH)], ANY),
    global("exists", &[of(PATH)], ANY),
    global("getAfter", &[of(PATH)], ANY),
    global("existsAfter", &[of(PATH)], ANY),
    global("value", &[of(PATH)], ANY),
    global("debug", &[ANY], ANY),
    global(
        "int",
        &[of(Types::of(&[T::Int, T::Float, T::String]))],
        of(INT),
    ),
    global(
        "float",
        &[of(Types::of(&[T::Int, T::Float, T::String]))],
        of(FLOAT),
    ),
    global(
        "string",
        &[of(Types::of(&[
            T::Int,
            T::Float,
            T::Null,
            T::String,
            T::Bool,
            T::Path,
        ]))],
        of(STRING),
    ),
    global("path", &[of(STRING)], of(PATH)),
    global("vector", &[of(LIST)], of(Types::of(&[T::Vector]))),
];

/// Functions in a namespace, as measured.
const NAMESPACED: &[Builtin] = &[
    global("math.abs", &[of(NUMBER)], of(NUMBER)),
    global("math.ceil", &[of(FLOAT_INT)], of(FLOAT)),
    global("math.floor", &[of(FLOAT_INT)], of(FLOAT)),
    global("math.isNaN", &[of(FLOAT_INT)], of(BOOL)),
    global("math.pow", &[of(FLOAT_INT), of(FLOAT_INT)], of(FLOAT)),
    global("math.round", &[of(FLOAT_INT)], of(INT)),
    global("math.sqrt", &[of(FLOAT_INT)], of(FLOAT)),
    global("duration.abs", &[of(DURATION)], of(DURATION)),
    global(
        "duration.time",
        &[of(INT), of(INT), of(INT), of(INT)],
        of(DURATION),
    ),
    global("duration.value", &[of(INT), of(STRING)], of(DURATION)),
    global(
        "timestamp.date",
        &[of(INT), of(INT), of(INT)],
        of(TIMESTAMP),
    ),
    global(
        "timestamp.value",
        &[of(Types::of(&[T::Int, T::String]))],
        of(TIMESTAMP),
    ),
    global("latlng.value", &[of(FLOAT_INT), of(FLOAT_INT)], of(LATLNG)),
    global(
        "hashing.crc32",
        &[of(Types::of(&[T::Bytes, T::String]))],
        of(BYTES),
    ),
    global(
        "hashing.crc32c",
        &[of(Types::of(&[T::Bytes, T::String]))],
        of(BYTES),
    ),
    global(
        "hashing.md5",
        &[of(Types::of(&[T::Bytes, T::String]))],
        of(BYTES),
    ),
    global(
        "hashing.sha256",
        &[of(Types::of(&[T::Bytes, T::String]))],
        of(BYTES),
    ),
];

/// Methods, as measured: the compiler knows them by name, whatever the receiver.
const METHODS: &[Builtin] = &[
    method(
        "get",
        MAP,
        &[of(Types::of(&[T::String, T::List])), ANY],
        ANY,
    ),
    method("size", SIZED, &[], of(INT)),
    method("matches", STRING, &[of(STRING)], of(BOOL)),
    method("keys", MAP, &[], of(LIST)),
    method("values", MAP, &[], of(LIST)),
    method("diff", MAP, &[of(MAP)], of(MAP_DIFF)),
    method("hasAll", LIST_SET, &[of(LIST_SET)], of(BOOL)),
    method("hasAny", LIST_SET, &[of(LIST_SET)], of(BOOL)),
    method("hasOnly", LIST_SET, &[of(LIST_SET)], of(BOOL)),
    method("toSet", LIST, &[], of(SET)),
    method("join", LIST, &[of(STRING)], of(STRING)),
    method("concat", LIST, &[of(LIST)], of(LIST)),
    method("removeAll", LIST, &[of(LIST)], of(LIST)),
    method("difference", SET, &[of(SET)], of(SET)),
    method("intersection", SET, &[of(SET)], of(SET)),
    method("union", SET, &[of(SET)], of(SET)),
    method("lower", STRING, &[], of(STRING)),
    method("upper", STRING, &[], of(STRING)),
    method("trim", STRING, &[], of(STRING)),
    method("split", STRING, &[of(STRING)], of(LIST)),
    method("replace", STRING, &[of(STRING), of(STRING)], of(STRING)),
    method("toUtf8", STRING, &[], of(BYTES)),
    method("toBase64", BYTES, &[], of(STRING)),
    method("toHexString", BYTES, &[], of(STRING)),
    method("date", TIMESTAMP, &[], of(TIMESTAMP)),
    method("time", TIMESTAMP, &[], of(DURATION)),
    method("toMillis", TIMESTAMP, &[], of(INT)),
    method("year", TIMESTAMP, &[], of(INT)),
    method("month", TIMESTAMP, &[], of(INT)),
    method("day", TIMESTAMP, &[], of(INT)),
    method("hours", TIMESTAMP, &[], of(INT)),
    method("minutes", TIMESTAMP, &[], of(INT)),
    method("dayOfWeek", TIMESTAMP, &[], of(INT)),
    method("dayOfYear", TIMESTAMP, &[], of(INT)),
    method("seconds", TIME, &[], of(INT)),
    method("nanos", TIME, &[], of(INT)),
    method("distance", LATLNG, &[of(LATLNG)], of(FLOAT)),
    method("latitude", LATLNG, &[], of(FLOAT)),
    method("longitude", LATLNG, &[], of(FLOAT)),
    method("bind", PATH, &[of(MAP)], of(PATH)),
    method("addedKeys", MAP_DIFF, &[], of(SET)),
    method("affectedKeys", MAP_DIFF, &[], of(SET)),
    method("changedKeys", MAP_DIFF, &[], of(SET)),
    method("removedKeys", MAP_DIFF, &[], of(SET)),
    method("unchangedKeys", MAP_DIFF, &[], of(SET)),
];

/// A form of an operator: its operands' types and its result's.
struct Form(&'static [Types], Types);

/// The forms of the arithmetic operators, in the order whose operand types the messages list.
const ADD: &[Form] = &[
    Form(&[NUMBER, NUMBER], NUMBER),
    Form(&[STRING, STRING], STRING),
    Form(&[DURATION, DURATION], DURATION),
    Form(&[DURATION, TIMESTAMP], TIMESTAMP),
    Form(&[TIMESTAMP, DURATION], TIMESTAMP),
];
const SUB: &[Form] = &[
    Form(&[NUMBER, NUMBER], NUMBER),
    Form(&[DURATION, DURATION], DURATION),
    Form(&[TIMESTAMP, DURATION], TIMESTAMP),
    Form(&[TIMESTAMP, TIMESTAMP], DURATION),
];
const MUL: &[Form] = &[Form(&[NUMBER, NUMBER], NUMBER)];
const NEG: &[Form] = &[Form(&[NUMBER], NUMBER), Form(&[DURATION], DURATION)];
const NOT: &[Form] = &[Form(&[BOOL], BOOL)];
const LOGIC: &[Form] = &[Form(&[BOOL, BOOL], BOOL)];

/// The result of an operator: every form's.
fn result_of(forms: &[Form]) -> Types {
    let mut out = Types::of(&[]);
    for form in forms {
        for &t in form.1.as_slice() {
            out.push(t);
        }
    }
    out
}

/// The types operand `i` may have for a result of `expected`: those of the forms that give one.
fn operand_of(forms: &[Form], i: usize, expected: Ty) -> Types {
    let mut out = Types::of(&[]);
    for form in forms {
        if expected.is_none_or(|e| form.1.meets(e)) {
            for &t in form.0[i].as_slice() {
                out.push(t);
            }
        }
    }
    out
}

/// A scope of function declarations: the top level, a service or a `match`.
struct Scope<'a> {
    parent: Option<usize>,
    /// Indexes into `Semantics::functions`, in source order, and the first of each name.
    functions: Vec<usize>,
    by_name: HashMap<&'a str, usize>,
    /// The variables of the `match` path, if this is one.
    wildcards: Vec<&'a Segment>,
    /// Their type: a string for a single segment, a path for a recursive wildcard, and, in a
    /// path with both, both for each variable (in the order of the segments, as measured).
    wildcard_type: Types,
}

struct Semantics<'a> {
    file: &'a FileAst,
    diags: Vec<Diag>,
    scopes: Vec<Scope<'a>>,
    /// Every function and the scope it was declared in.
    functions: Vec<(&'a Function, usize)>,
    used: Vec<bool>,
    /// Functions declared twice in a scope (an error): calls of them are not typed.
    duplicated: Vec<bool>,
    calls: Vec<Vec<usize>>,
    /// The scope of every `match`, in the order `declare_body` met them, and how many of them
    /// `body` has visited again.
    match_scopes: Vec<usize>,
    visited: usize,
    /// Per function, which of its parameters and `let`s it returns, and its result's type.
    returns: Vec<Option<Rc<Vec<bool>>>>,
    results: Vec<Option<Ty>>,
    /// The types a call expecting a result of some type requires of each argument, worked out
    /// once per function and type, so that the time is bounded by the functions whatever the
    /// calls. Within a recursion (an error), a call of a function and type being worked out
    /// takes any arguments: what depends on it is kept only until that one is done
    /// (`tentative`, with the depth it depends on, and by that depth in `dependents`).
    /// `working` holds the signatures being worked out, by depth, and `lows` the lowest depth
    /// each depends on. Past `budget` signatures, in a long cycle of calls, what depends on one
    /// being worked out is kept as it is.
    signatures: HashMap<Call, Rc<Vec<Ty>>>,
    tentative: HashMap<Call, (Rc<Vec<Ty>>, usize)>,
    dependents: Vec<Vec<Call>>,
    working: HashMap<Call, usize>,
    lows: Vec<usize>,
    budget: usize,
    /// Functions whose result is being worked out, and how many calls are being followed.
    busy: Vec<bool>,
    following: usize,
}

/// The `match`es around a body, for the compiler's limits.
#[derive(Debug, Clone, Copy, Default)]
struct Nesting {
    /// How many `match`es enclose it (1 inside a service's first).
    depth: usize,
    /// Their path segments and variables, all together.
    segments: usize,
    variables: usize,
    /// Whether the path length or the variables were reported already.
    long: bool,
    many: bool,
}

const MAX_MATCH_DEPTH: usize = 10;
const MAX_PATH_LENGTH: usize = 100;
const MAX_MATCH_VARIABLES: usize = 20;
const MAX_ARGUMENTS: usize = 7;
const MAX_CALL_DEPTH: usize = 20;
/// How many signatures per function may be worked out before those of a cycle of calls are
/// kept as they are (a ruleset without cycles works out each once).
const SIGNATURES_PER_FUNCTION: usize = 16;
/// How many calls deep the types of arguments and results are followed: past the call depth
/// the compiler allows (an error), the rest of the chain takes any type.
const MAX_FOLLOWED_CALLS: usize = 64;

/// A parameter or `let` while a function is checked.
struct Local<'a> {
    name: &'a str,
    /// What its uses require so far.
    ty: Ty,
    used: bool,
}

/// Where an expression is checked.
struct Cx<'a> {
    scope: usize,
    /// The function being checked, if any.
    caller: Option<usize>,
    /// Whether to report what is found, or only learn the types of parameters and `let`s.
    report: bool,
    /// The function's parameters, then its `let`s.
    locals: Vec<Local<'a>>,
    params: usize,
    /// How many of `locals` the expression sees.
    visible: usize,
    /// The start of the statement being checked, where a variable's conflicting uses are
    /// reported, and the variables reported there.
    statement: Span,
    conflicts: Vec<(usize, usize)>,
}

impl<'a> Cx<'a> {
    fn new(scope: usize, caller: Option<usize>, report: bool) -> Self {
        Self {
            scope,
            caller,
            report,
            locals: Vec::new(),
            params: 0,
            visible: 0,
            statement: Span::default(),
            conflicts: Vec::new(),
        }
    }

    /// The `let` or parameter `name`: the last `let` of the name, or else the first
    /// parameter.
    fn local(&self, name: &str) -> Option<usize> {
        match self.locals[self.params..self.visible]
            .iter()
            .rposition(|l| l.name == name)
        {
            Some(k) => Some(self.params + k),
            None => self.locals[..self.params]
                .iter()
                .position(|l| l.name == name),
        }
    }
}

/// How an expression is used, for where the compiler reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// The condition of an `allow`.
    Root,
    Operand,
    /// What a field, index, slice or method is applied to, or what `-` negates: one pair of
    /// parentheses around it does not count.
    Receiver,
}

pub(crate) fn semantics(file: &FileAst, version: Version, diags: &mut Vec<Diag>) {
    let mut s = Semantics {
        file,
        diags: Vec::new(),
        scopes: vec![Scope {
            parent: None,
            functions: Vec::new(),
            by_name: HashMap::new(),
            wildcards: Vec::new(),
            wildcard_type: Types::of(&[]),
        }],
        functions: Vec::new(),
        used: Vec::new(),
        duplicated: Vec::new(),
        calls: Vec::new(),
        match_scopes: Vec::new(),
        visited: 0,
        returns: Vec::new(),
        results: Vec::new(),
        signatures: HashMap::new(),
        tentative: HashMap::new(),
        dependents: Vec::new(),
        working: HashMap::new(),
        lows: Vec::new(),
        budget: 0,
        busy: Vec::new(),
        following: 0,
    };
    if version == Version::V1 {
        s.diags.push(Diag {
            span: None,
            message: "Ruleset uses old version (version [1]). Please update to the latest \
                      version (version [2])."
                .to_owned(),
            severity: crate::Severity::Warning,
        });
    }
    // Declare every function first: a function may be called before its declaration.
    for f in &file.functions {
        s.declare(f, 0);
    }
    let mut services: Vec<(&str, usize)> = Vec::new();
    let mut seen_services: Vec<&str> = Vec::new();
    for service in &file.services {
        if seen_services.contains(&service.name.as_str()) {
            s.diags.push(Diag::error(
                service.name_span,
                "Duplicate service name declaration.",
            ));
        }
        seen_services.push(&service.name);
        if !SERVICES.contains(&service.name.as_str()) {
            s.diags.push(Diag::warning(
                service.name_span,
                format!("Invalid service name: {}.", service.name),
            ));
        }
        let scope = s.scope(0, Vec::new());
        s.declare_body(&service.body, scope);
        services.push((&service.name, scope));
    }
    s.duplicates();
    let functions = s.functions.len();
    s.returns = vec![None; functions];
    s.results = vec![None; functions];
    s.busy = vec![false; functions];
    s.budget = SIGNATURES_PER_FUNCTION * functions;

    let mut lets = Vec::new();
    for i in 0..functions {
        s.function(i, &mut lets);
    }
    if version == Version::V1 {
        for name in lets {
            s.diags.push(Diag::error(
                name,
                "Let bindings are not allowed in rules_version=\"1\";",
            ));
        }
    }
    for (service, (name, scope)) in file.services.iter().zip(services) {
        s.body(&service.body, scope, SERVICES.contains(&name), version);
    }
    for (i, (f, _)) in s.functions.iter().enumerate() {
        if !s.used[i] {
            s.diags.push(Diag::warning(
                f.name.span,
                format!("Unused function: {}.", f.name.name),
            ));
        }
    }
    s.cycles();
    diags.append(&mut s.diags);
}

impl<'a> Semantics<'a> {
    fn scope(&mut self, parent: usize, wildcards: Vec<&'a Segment>) -> usize {
        let mut wildcard_type = Types::of(&[]);
        for segment in &wildcards {
            match segment.kind {
                SegmentKind::Wildcard(_) => wildcard_type.push(T::String),
                SegmentKind::Recursive(_) => wildcard_type.push(T::Path),
                SegmentKind::Literal(_) => {}
            }
        }
        self.scopes.push(Scope {
            parent: Some(parent),
            functions: Vec::new(),
            by_name: HashMap::new(),
            wildcards,
            wildcard_type,
        });
        self.scopes.len() - 1
    }

    fn declare(&mut self, f: &'a Function, scope: usize) {
        let s = &mut self.scopes[scope];
        s.functions.push(self.functions.len());
        s.by_name
            .entry(&f.name.name)
            .or_insert(self.functions.len());
        self.functions.push((f, scope));
        self.used.push(false);
        self.calls.push(Vec::new());
    }

    /// Declares the functions of a body and, recursively, of its `match`es, whose scopes are
    /// created in the order `body` visits them.
    fn declare_body(&mut self, body: &'a Body, scope: usize) {
        for f in &body.functions {
            self.declare(f, scope);
        }
        for m in &body.matches {
            let inner = self.scope(scope, m.path.iter().collect());
            self.match_scopes.push(inner);
            self.declare_body(&m.body, inner);
        }
    }

    /// "Function f is already defined." for a second declaration in one scope.
    /// A second declaration is never called (calls find the first), and is not reported unused.
    fn duplicates(&mut self) {
        self.duplicated = vec![false; self.functions.len()];
        for scope in &self.scopes {
            for &i in &scope.functions {
                let name = &self.functions[i].0.name;
                let first = scope.by_name[name.name.as_str()];
                if first != i {
                    self.diags.push(Diag::error(
                        name.span,
                        format!("Function {} is already defined.", name.name),
                    ));
                    self.used[i] = true;
                    self.duplicated[i] = true;
                    self.duplicated[first] = true;
                }
            }
        }
    }

    /// The function `name` as seen from `scope`.
    fn resolve_function(&self, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            if let Some(&found) = self.scopes[scope].by_name.get(name) {
                return Some(found);
            }
            scope = self.scopes[scope].parent?;
        }
    }

    /// The type of the wildcard `name` as seen from `scope`.
    fn resolve_wildcard(&self, mut scope: usize, name: &str) -> Option<Types> {
        loop {
            let s = &self.scopes[scope];
            if s.wildcards.iter().any(|w| w.kind.variable() == Some(name)) {
                return Some(s.wildcard_type);
            }
            scope = s.parent?;
        }
    }

    /// Where the body of function `i` is checked: its parameters and `let`s, none of them
    /// used or typed yet, all visible.
    fn function_cx(&self, i: usize, report: bool) -> Cx<'a> {
        let (f, scope) = self.functions[i];
        let mut cx = Cx::new(scope, Some(i), report);
        cx.locals = f
            .params
            .iter()
            .map(|p| p.name.as_str())
            .chain(f.lets.iter().map(|l| l.name.name.as_str()))
            .map(|name| Local {
                name,
                ty: ANY,
                used: false,
            })
            .collect();
        cx.params = f.params.len();
        cx.visible = cx.locals.len();
        cx
    }

    /// Checks function `i`, reporting what it finds; the names of its `let`s go to `lets`.
    fn function(&mut self, i: usize, lets: &mut Vec<Span>) {
        let f = self.functions[i].0;
        if f.params.len() > MAX_ARGUMENTS {
            self.diags.push(Diag::error(
                f.name.span,
                format!(
                    "Maximum allowed function arguments of {MAX_ARGUMENTS} for function {} is \
                     reached.",
                    f.name.name
                ),
            ));
        }
        let mut cx = self.function_cx(i, true);
        // A repeated parameter is an error, and is not reported unused: the name is the first.
        for (k, p) in f.params.iter().enumerate() {
            if f.params[..k].iter().any(|q| q.name == p.name) {
                cx.locals[k].used = true;
                self.diags.push(Diag::error(
                    p.span,
                    format!("Variable {} is already defined.", p.name),
                ));
            }
        }
        for (k, l) in f.lets.iter().enumerate() {
            lets.push(l.name.span);
            let earlier = f
                .params
                .iter()
                .map(|p| p.name.as_str())
                .chain(f.lets[..k].iter().map(|l| l.name.name.as_str()));
            if earlier.clone().any(|name| name == l.name.name) {
                self.diags.push(Diag::error(
                    l.name.span,
                    format!(
                        "The identifier [{}] is already bound in the current scope.",
                        l.name.name
                    ),
                ));
            }
        }
        self.walk_body(i, &mut cx);
        let spans = f
            .params
            .iter()
            .map(|p| p.span)
            .chain(f.lets.iter().map(|l| l.name.span));
        for (local, span) in cx.locals.iter().zip(spans) {
            if !local.used {
                self.diags.push(Diag::warning(
                    span,
                    format!("Unused variable: {}.", local.name),
                ));
            }
        }
    }

    /// Checks the body of function `i`: its result, then its `let`s from the last, each
    /// against what the statements after it require of it.
    fn walk_body(&mut self, i: usize, cx: &mut Cx<'a>) {
        let f = self.functions[i].0;
        cx.visible = cx.locals.len();
        cx.statement = self.first_token(&f.result);
        self.check(&f.result, ANY, Slot::Operand, cx);
        for (k, l) in f.lets.iter().enumerate().rev() {
            let local = cx.params + k;
            cx.visible = local;
            cx.statement = l.span;
            let ty = cx.locals[local].ty;
            self.check(&l.value, ty, Slot::Operand, cx);
        }
    }

    /// The function a call `name(args)` calls, if it is one of the ruleset's with as many
    /// parameters.
    fn user_call(&self, scope: usize, name: &Ident, args: &[Expr]) -> Option<usize> {
        let g = self.resolve_function(scope, &name.name)?;
        (self.functions[g].0.params.len() == args.len() && !self.duplicated[g]).then_some(g)
    }

    /// Which parameters and `let`s of function `i` it returns as they are: directly, through
    /// `let`s and conditionals, or as an argument another function returns.
    fn returns(&mut self, i: usize) -> Rc<Vec<bool>> {
        if let Some(found) = &self.returns[i] {
            return Rc::clone(found);
        }
        let f = self.functions[i].0;
        let mut cx = self.function_cx(i, false);
        let mut returned = vec![false; cx.locals.len()];
        if self.busy[i] || self.following == MAX_FOLLOWED_CALLS {
            return Rc::new(returned);
        }
        self.busy[i] = true;
        self.following += 1;
        let mut pending = vec![(&f.result, cx.visible)];
        while let Some((e, visible)) = pending.pop() {
            cx.visible = visible;
            match &e.kind {
                ExprKind::Ternary {
                    then, otherwise, ..
                } => {
                    pending.push((then, visible));
                    pending.push((otherwise, visible));
                }
                ExprKind::Ident(name) => {
                    if let Some(local) = cx.local(name) {
                        if !returned[local] && local >= cx.params {
                            pending.push((&f.lets[local - cx.params].value, local));
                        }
                        returned[local] = true;
                    }
                }
                ExprKind::Call {
                    callee: Callee::Global(name),
                    args,
                } => {
                    if let Some(g) = self.user_call(cx.scope, name, args) {
                        let theirs = self.returns(g);
                        for (arg, _) in args.iter().zip(theirs.iter()).filter(|(_, r)| **r) {
                            pending.push((arg, visible));
                        }
                    }
                }
                _ => {}
            }
        }
        self.busy[i] = false;
        self.following -= 1;
        let found = Rc::new(returned);
        self.returns[i] = Some(Rc::clone(&found));
        found
    }

    /// The types a call of function `i` whose result is expected to be of type `ty` requires
    /// of its arguments. Its body is checked as usual, its result expected to be of `ty`; a
    /// parameter it returns as it is takes `ty` last.
    fn signature(&mut self, i: usize, ty: Ty) -> Rc<Vec<Ty>> {
        let key = (i, ty);
        if let Some(found) = self.signatures.get(&key) {
            return Rc::clone(found);
        }
        let f = self.functions[i].0;
        if let Some((found, low)) = self.tentative.get(&key) {
            let (found, low) = (Rc::clone(found), *low);
            self.depends_on(low);
            return found;
        }
        if let Some(&depth) = self.working.get(&key) {
            self.depends_on(depth);
            return Rc::new(vec![ANY; f.params.len()]);
        }
        if self.following == MAX_FOLLOWED_CALLS {
            return Rc::new(vec![ANY; f.params.len()]);
        }
        self.following += 1;
        let depth = self.lows.len();
        self.budget = self.budget.saturating_sub(1);
        self.lows.push(depth);
        if self.dependents.len() == depth {
            self.dependents.push(Vec::new());
        }
        self.working.insert(key, depth);
        let returned = self.returns(i);
        let mut cx = self.function_cx(i, false);
        // What the parameters returned as they are take last, and what the `let`s returned as
        // they are take.
        let mut last = Vec::new();
        let mut lets = vec![None; f.lets.len()];
        self.flow(&f.result, ty, &mut cx, &returned, &mut last, &mut lets);
        for (k, l) in f.lets.iter().enumerate().rev() {
            let local = cx.params + k;
            cx.visible = local;
            match lets[k] {
                Some(returned_ty) => {
                    let ty = fold(cx.locals[local].ty, returned_ty);
                    let mut nested = vec![None; f.lets.len()];
                    self.flow(&l.value, ty, &mut cx, &returned, &mut last, &mut nested);
                    for (k, ty) in nested.into_iter().enumerate() {
                        if let Some(ty) = ty {
                            lets[k] = Some(lets[k].map_or(ty, |t| fold(t, ty)));
                        }
                    }
                }
                None => {
                    let ty = cx.locals[local].ty;
                    self.check(&l.value, ty, Slot::Operand, &mut cx);
                }
            }
        }
        for (local, ty) in last {
            self.constrain(local, ty, &mut cx);
        }
        self.working.remove(&key);
        self.following -= 1;
        let low = self.lows.pop().unwrap_or(depth);
        let found = Rc::new(cx.locals[..cx.params].iter().map(|l| l.ty).collect());
        // What depended on this one is worked out again when needed.
        for dependent in std::mem::take(&mut self.dependents[depth]) {
            self.tentative.remove(&dependent);
        }
        if low < depth && self.budget > 0 {
            self.tentative.insert(key, (Rc::clone(&found), low));
            self.dependents[low].push(key);
            self.depends_on(low);
        } else {
            self.signatures.insert(key, Rc::clone(&found));
        }
        found
    }

    /// The signature being worked out depends on the one at `depth`.
    fn depends_on(&mut self, depth: usize) {
        if let Some(low) = self.lows.last_mut() {
            *low = (*low).min(depth);
        }
    }

    /// The part of a function's result `e`, expected to be of type `ty`. A parameter returned
    /// as it is goes to `last`, a `let` to `lets`; the rest is checked.
    fn flow(
        &mut self,
        e: &'a Expr,
        ty: Ty,
        cx: &mut Cx<'a>,
        returned: &[bool],
        last: &mut Vec<(usize, Types)>,
        lets: &mut [Option<Ty>],
    ) {
        if let ExprKind::Ternary {
            condition,
            then,
            otherwise,
            ..
        } = &e.kind
        {
            self.check(condition, Some(BOOL), Slot::Operand, cx);
            self.flow(then, ty, cx, returned, last, lets);
            self.flow(otherwise, ty, cx, returned, last, lets);
            return;
        }
        if let ExprKind::Ident(name) = &e.kind
            && let Some(local) = cx.local(name)
            && returned[local]
        {
            if local < cx.params {
                last.extend(ty.map(|ty| (local, ty)));
            } else {
                let k = local - cx.params;
                lets[k] = Some(lets[k].map_or(ty, |t| fold(t, ty)));
            }
            return;
        }
        if let ExprKind::Call {
            callee: Callee::Global(name),
            args,
        } = &e.kind
            && let Some(g) = self.user_call(cx.scope, name, args)
        {
            let theirs = self.returns(g);
            let types = self.signature(g, ty);
            for (k, arg) in args.iter().enumerate() {
                if theirs[k] {
                    self.flow(arg, types[k], cx, returned, last, lets);
                } else {
                    self.check(arg, types[k], Slot::Operand, cx);
                }
            }
            return;
        }
        self.check(e, ty, Slot::Operand, cx);
    }

    /// The type of the result of function `i`, for comparisons.
    fn result_type(&mut self, i: usize) -> Ty {
        if let Some(ty) = self.results[i] {
            return ty;
        }
        if self.busy[i] || self.following == MAX_FOLLOWED_CALLS {
            return ANY;
        }
        self.busy[i] = true;
        self.following += 1;
        let cx = self.function_cx(i, false);
        let ty = self.type_of(&self.functions[i].0.result, &cx);
        self.busy[i] = false;
        self.following -= 1;
        self.results[i] = Some(ty);
        ty
    }

    /// Adds a use requiring `ty` to a parameter or `let`. When none of its types is left, the
    /// compiler reports it at the statement, once, and the variable takes any type again.
    fn constrain(&mut self, local: usize, ty: Types, cx: &mut Cx<'a>) {
        let current = &mut cx.locals[local].ty;
        match *current {
            None => *current = Some(ty),
            Some(t) if !t.filter(ty).is_empty() => *current = Some(t.filter(ty)),
            Some(_) => {
                *current = None;
                let key = (local, cx.statement.start);
                if cx.report && !cx.conflicts.contains(&key) {
                    cx.conflicts.push(key);
                    self.diags.push(Diag::warning(
                        cx.statement,
                        format!(
                            "Invalid type. There is no type for variable '{}' that satisfies \
                             all clauses.",
                            cx.locals[local].name
                        ),
                    ));
                }
            }
        }
    }

    /// Checks the `allow`s of a body and of its `match`es, in the order `declare_body` met
    /// them.
    fn body(&mut self, body: &'a Body, scope: usize, known_service: bool, version: Version) {
        self.body_at(body, scope, known_service, version, Nesting::default());
    }

    fn body_at(
        &mut self,
        body: &'a Body,
        scope: usize,
        known_service: bool,
        version: Version,
        outer: Nesting,
    ) {
        for a in &body.allows {
            for m in &a.methods {
                if !known_service || m.method.is_none() {
                    self.diags.push(Diag::warning(
                        m.span,
                        format!("Invalid method name: {}.", m.name),
                    ));
                }
            }
            if let Some(c) = &a.condition {
                let mut cx = Cx::new(scope, None, true);
                self.check(c, Some(BOOL), Slot::Root, &mut cx);
            }
        }
        for m in &body.matches {
            let inner = self.match_scopes[self.visited];
            self.visited += 1;
            if version == Version::V1
                && !m.body.matches.is_empty()
                && m.path
                    .iter()
                    .any(|s| matches!(s.kind, SegmentKind::Recursive(_)))
            {
                self.diags.push(Diag::warning(
                    m.span,
                    "Glob(**) resource rules should not have child resource rules.",
                ));
            }
            let nesting = self.limits(m, outer);
            self.body_at(&m.body, inner, known_service, version, nesting);
        }
    }

    /// The compiler's limits on `match`es: how deep they nest, how long their paths get and how
    /// many variables they bind, counting the enclosing ones. Each is reported once on a path.
    fn limits(&mut self, m: &Match, outer: Nesting) -> Nesting {
        let mut n = Nesting {
            depth: outer.depth + 1,
            segments: outer.segments + m.path.len(),
            variables: outer.variables
                + m.path
                    .iter()
                    .filter(|s| s.kind.variable().is_some())
                    .count(),
            ..outer
        };
        // The first `match` of a service is at depth 0.
        if n.depth == MAX_MATCH_DEPTH + 2 {
            self.diags.push(Diag::error(
                m.span,
                format!("Maximum allowed resource rule depth of {MAX_MATCH_DEPTH} is reached."),
            ));
        }
        if n.segments > MAX_PATH_LENGTH && !n.long {
            n.long = true;
            self.diags.push(Diag::error(
                m.span,
                format!("Maximum allowed path length of {MAX_PATH_LENGTH} is reached."),
            ));
        }
        if n.variables > MAX_MATCH_VARIABLES && !n.many {
            n.many = true;
            self.diags.push(Diag::error(
                m.span,
                format!(
                    "Maximum allowed amount of {MAX_MATCH_VARIABLES} match field variables is \
                     reached."
                ),
            ));
        }
        n
    }

    // ----- types

    /// The type an expression has, whatever it is expected to be: a parameter, a `let`, a
    /// field, a conditional and the result of `get()` have any type.
    fn type_of(&mut self, e: &'a Expr, cx: &Cx<'a>) -> Ty {
        Some(match &e.kind {
            ExprKind::Null => Types::of(&[T::Null]),
            ExprKind::Bool(_) => BOOL,
            ExprKind::Int(_) | ExprKind::IntOverflow(_) => INT,
            ExprKind::Float(_) => FLOAT,
            ExprKind::String(_) => STRING,
            ExprKind::Bytes(_) => BYTES,
            ExprKind::List(_) => LIST,
            ExprKind::Map(_) => MAP,
            ExprKind::Path(_) => PATH,
            ExprKind::Ident(name) => {
                if cx.local(name).is_some() {
                    return ANY;
                }
                return self.resolve_wildcard(cx.scope, name);
            }
            ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::Ternary { .. } => {
                return ANY;
            }
            ExprKind::Slice { .. } => SLICED,
            // A call with the wrong number of arguments may return anything.
            ExprKind::Call { callee, args } => {
                let b = match callee {
                    Callee::Global(name) => match self.resolve_function(cx.scope, &name.name) {
                        Some(_) => {
                            return match self.user_call(cx.scope, name, args) {
                                Some(g) => self.result_type(g),
                                None => ANY,
                            };
                        }
                        None => builtin(GLOBALS, &name.name),
                    },
                    Callee::Namespaced { namespace, name } => {
                        builtin(NAMESPACED, &format!("{}.{}", namespace.name, name.name))
                    }
                    Callee::Method { name, .. } => builtin(METHODS, &name.name),
                };
                return b
                    .filter(|b| b.params.len() == args.len())
                    .and_then(|b| b.result);
            }
            ExprKind::Unary { op, .. } => match op {
                UnaryOp::Not => BOOL,
                UnaryOp::Neg => result_of(NEG),
            },
            ExprKind::Binary { op, .. } => match op {
                BinaryOp::Add => result_of(ADD),
                BinaryOp::Sub => result_of(SUB),
                BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => result_of(MUL),
                _ => BOOL,
            },
            ExprKind::Is { .. } => BOOL,
        })
    }

    /// Checks `e` where a value of type `expected` is expected, and what is in it. Returns
    /// whether `e` itself can be of that type. If it cannot, it is reported (and only what is
    /// in it regardless of how `e` is used is checked).
    fn check(&mut self, e: &'a Expr, expected: Ty, slot: Slot, cx: &mut Cx<'a>) -> bool {
        let found = self.type_of(e, cx);
        let fits = match (found, expected) {
            (Some(found), Some(expected)) => found.meets(expected),
            _ => true,
        };
        // Variables, conditionals and calls are reported where they are handled.
        let reported_here = !matches!(
            e.kind,
            ExprKind::Ident(_) | ExprKind::Ternary { .. } | ExprKind::Call { .. }
        );
        if !fits && reported_here {
            self.mismatch(e, slot, found, expected, cx);
        }
        // Below a node that does not fit, operands are not expected to be of any type.
        let within = if fits { expected } else { ANY };
        match &e.kind {
            ExprKind::Null
            | ExprKind::Bool(_)
            | ExprKind::Int(_)
            | ExprKind::IntOverflow(_)
            | ExprKind::Float(_)
            | ExprKind::String(_)
            | ExprKind::Bytes(_) => {}
            ExprKind::Ident(name) => return self.variable(e, name, expected, slot, cx),
            ExprKind::List(items) => {
                for item in items {
                    self.check(item, ANY, Slot::Operand, cx);
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    self.check(k, Some(STRING), Slot::Operand, cx);
                    self.check(v, ANY, Slot::Operand, cx);
                }
            }
            ExprKind::Path(parts) => {
                for part in parts {
                    if let PathPart::Expr(e) = part {
                        self.check(e, Some(PATH_PART), Slot::Operand, cx);
                    }
                }
            }
            ExprKind::Member { object, .. } => {
                self.check(object, Some(MAP), Slot::Receiver, cx);
            }
            ExprKind::Index { object, index } => {
                // A string's character is a string; the other values' elements may be anything.
                let objects = match within {
                    Some(e) if !e.contains(T::String) => Types::of(&[T::List, T::Map, T::Path]),
                    _ => INDEXED,
                };
                self.check(object, Some(objects), Slot::Receiver, cx);
                self.check(index, Some(KEY), Slot::Operand, cx);
            }
            ExprKind::Slice { object, start, end } => {
                if fits {
                    let objects = within.map_or(SLICED, |e| SLICED.filter(e));
                    self.check(object, Some(objects), Slot::Receiver, cx);
                    self.check(start, Some(BOUND), Slot::Operand, cx);
                    self.check(end, Some(BOUND), Slot::Operand, cx);
                } else {
                    self.check(object, ANY, Slot::Receiver, cx);
                    self.check(start, ANY, Slot::Operand, cx);
                    self.check(end, ANY, Slot::Operand, cx);
                }
            }
            ExprKind::Call { callee, args } => {
                return self.call(e, callee, args, (found, expected), slot, cx);
            }
            ExprKind::Unary { op, operand } => {
                let (forms, slot) = match op {
                    UnaryOp::Not => (NOT, Slot::Operand),
                    UnaryOp::Neg => (NEG, Slot::Receiver),
                };
                let operand_ty = fits.then(|| operand_of(forms, 0, within));
                self.check(operand, operand_ty, slot, cx);
            }
            ExprKind::Binary {
                op,
                op_span,
                left,
                right,
            } => {
                let forms = match op {
                    BinaryOp::Add => Some(ADD),
                    BinaryOp::Sub => Some(SUB),
                    BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => Some(MUL),
                    BinaryOp::And | BinaryOp::Or => Some(LOGIC),
                    _ => None,
                };
                match (forms, op) {
                    (Some(forms), _) => {
                        let left_ty = fits.then(|| operand_of(forms, 0, within));
                        let right_ty = fits.then(|| operand_of(forms, 1, within));
                        self.check(left, left_ty, Slot::Operand, cx);
                        self.check(right, right_ty, Slot::Operand, cx);
                    }
                    (None, BinaryOp::In) => {
                        self.check(left, ANY, Slot::Operand, cx);
                        self.check(right, fits.then_some(CONTAINER), Slot::Operand, cx);
                    }
                    (None, BinaryOp::Eq | BinaryOp::Ne) => {
                        self.check(left, ANY, Slot::Operand, cx);
                        self.check(right, ANY, Slot::Operand, cx);
                        let l = self.type_of(left, cx);
                        let r = self.type_of(right, cx);
                        if let (Some(l), Some(r)) = (l, r)
                            && !l.comparable(r)
                            && fits
                            && cx.report
                        {
                            let result = if *op == BinaryOp::Eq { "false" } else { "true" };
                            self.diags.push(Diag::warning(
                                *op_span,
                                format!(
                                    "The sub-expressions are not comparable, so this \
                                     comparison will always return {result}."
                                ),
                            ));
                        }
                    }
                    (None, _) => {
                        self.check(left, ANY, Slot::Operand, cx);
                        self.check(right, ANY, Slot::Operand, cx);
                    }
                }
            }
            ExprKind::Is { value, .. } => {
                self.check(value, ANY, Slot::Operand, cx);
            }
            ExprKind::Ternary {
                condition,
                then,
                otherwise,
                ..
            } => {
                self.check(condition, Some(BOOL), Slot::Operand, cx);
                let a = self.check(then, expected, Slot::Operand, cx);
                let b = self.check(otherwise, expected, Slot::Operand, cx);
                let (ta, tb) = (self.type_of(then, cx), self.type_of(otherwise, cx));
                if a && b
                    && let (Some(ta), Some(tb)) = (ta, tb)
                    && !ta.meets(tb)
                    && cx.report
                {
                    let at = self.token(e);
                    self.diags.push(Diag::warning(
                        at,
                        "Ternary operator result if condition is true has a different type \
                         than if condition is false.",
                    ));
                }
                return true;
            }
        }
        fits
    }

    /// A variable where a value of type `expected` is expected.
    fn variable(
        &mut self,
        e: &'a Expr,
        name: &str,
        expected: Ty,
        slot: Slot,
        cx: &mut Cx<'a>,
    ) -> bool {
        if let Some(local) = cx.local(name) {
            if cx.report {
                cx.locals[local].used = true;
            }
            if let Some(expected) = expected {
                self.constrain(local, expected, cx);
            }
            return true;
        }
        if let Some(ty) = self.resolve_wildcard(cx.scope, name) {
            let fits = expected.is_none_or(|e| ty.meets(e));
            if !fits && cx.report {
                let at = self.position(e, slot);
                self.mismatch(e, slot, Some(ty), expected, cx);
                self.diags.push(Diag::warning(
                    at,
                    format!(
                        "Invalid type. There is no type for variable '{name}' that satisfies \
                         all clauses."
                    ),
                ));
            }
            return fits;
        }
        if !matches!(name, "request" | "resource") && !is_package(name) && cx.report {
            self.diags.push(Diag::warning(
                e.span,
                format!("Invalid variable name: {name}."),
            ));
        }
        true
    }

    /// "Invalid type. Received one of [found]. Expected one of [expected]." at `e`.
    fn mismatch(&mut self, e: &Expr, slot: Slot, found: Ty, expected: Ty, cx: &Cx<'a>) {
        let (Some(found), Some(expected), true) = (found, expected, cx.report) else {
            return;
        };
        let at = self.position(e, slot);
        let message = if slot == Slot::Root {
            format!(
                "Invalid type. Permission statements expect a boolean value. Received one of \
                 [{}].",
                found.list()
            )
        } else {
            format!(
                "Invalid type. Received one of [{}]. Expected one of [{}].",
                found.list(),
                expected.list()
            )
        };
        self.diags.push(Diag::warning(at, message));
    }

    /// A call where a value of type `expected` is expected (`found` is the type of its
    /// result). A call of one of the ruleset's functions takes the expected type to what the
    /// function returns; a built-in one with the wrong number of arguments, or whose result
    /// does not fit, takes any receiver and arguments.
    fn call(
        &mut self,
        e: &'a Expr,
        callee: &'a Callee,
        args: &'a [Expr],
        (found, expected): (Ty, Ty),
        slot: Slot,
        cx: &mut Cx<'a>,
    ) -> bool {
        let (name, full, table, receiver) = match callee {
            Callee::Global(name) => {
                if let Some(f) = self.resolve_function(cx.scope, &name.name) {
                    self.function_call(f, name, args, expected, cx);
                    return true;
                }
                (name, name.name.clone(), GLOBALS, None)
            }
            Callee::Namespaced { namespace, name } => (
                name,
                format!("{}.{}", namespace.name, name.name),
                NAMESPACED,
                None,
            ),
            Callee::Method { receiver, name } => {
                (name, name.name.clone(), METHODS, Some(&**receiver))
            }
        };
        let Some(b) = builtin(table, &full) else {
            self.unknown_function(name, &full, cx);
            if let Some(receiver) = receiver {
                self.check(receiver, ANY, Slot::Receiver, cx);
            }
            for a in args {
                self.check(a, ANY, Slot::Operand, cx);
            }
            return true;
        };
        if cx.report && receiver.is_some() && matches!(full.as_str(), "matches" | "split") {
            self.regex(args);
        }
        let arity = b.params.len() == args.len();
        if !arity {
            self.arity(name, &full, cx);
        }
        let fits = match (found, expected) {
            (Some(found), Some(expected)) => found.meets(expected),
            _ => true,
        };
        if !fits {
            self.mismatch(e, slot, found, expected, cx);
        }
        let typed = arity && fits;
        if let Some(receiver) = receiver {
            self.check(receiver, typed.then_some(b.receiver), Slot::Receiver, cx);
        }
        for (k, a) in args.iter().enumerate() {
            let ty = if typed { b.params[k] } else { ANY };
            self.check(a, ty, Slot::Operand, cx);
        }
        fits
    }

    /// A call of the ruleset's function `f`: each argument is expected to be of the type the
    /// function's body and the use of its result require.
    fn function_call(
        &mut self,
        f: usize,
        name: &Ident,
        args: &'a [Expr],
        expected: Ty,
        cx: &mut Cx<'a>,
    ) {
        if cx.report {
            self.used[f] = true;
            if let Some(caller) = cx.caller {
                self.calls[caller].push(f);
            }
        }
        let arity = self.functions[f].0.params.len() == args.len();
        if !arity {
            self.arity(name, &name.name, cx);
        }
        if !arity || self.duplicated[f] {
            for a in args {
                self.check(a, ANY, Slot::Operand, cx);
            }
            return;
        }
        let types = self.signature(f, expected);
        for (a, ty) in args.iter().zip(types.iter()) {
            self.check(a, *ty, Slot::Operand, cx);
        }
    }

    fn arity(&mut self, at: &Ident, name: &str, cx: &Cx<'a>) {
        if cx.report {
            self.diags.push(Diag::warning(
                at.span,
                format!("Incorrect number of arguments supplied to function: {name}."),
            ));
        }
    }

    fn unknown_function(&mut self, at: &Ident, name: &str, cx: &Cx<'a>) {
        if cx.report {
            self.diags.push(Diag::warning(
                at.span,
                format!("Invalid function name: {name}."),
            ));
        }
    }

    // ----- positions

    /// The parentheses around `e`, outermost first.
    fn parens(&self, e: &Expr) -> &[Span] {
        self.file
            .parens
            .get(&(e.span.start, e.span.end))
            .map_or(&[], Vec::as_slice)
    }

    /// The token the compiler reports `e` at: a literal or variable itself (or the parenthesis
    /// around it), an operator, a function's name, the start of what a field, index or slice
    /// is taken of.
    fn position(&self, e: &Expr, slot: Slot) -> Span {
        let parens = self.parens(e);
        let simple = matches!(
            e.kind,
            ExprKind::Null
                | ExprKind::Bool(_)
                | ExprKind::Int(_)
                | ExprKind::IntOverflow(_)
                | ExprKind::Float(_)
                | ExprKind::String(_)
                | ExprKind::Bytes(_)
                | ExprKind::Ident(_)
                | ExprKind::List(_)
                | ExprKind::Map(_)
                | ExprKind::Path(_)
        );
        match (simple, slot, parens) {
            (true, Slot::Receiver, [_, inner, ..]) => *inner,
            (true, Slot::Receiver, _) | (true, _, []) | (false, _, _) => self.token(e),
            (true, _, [outer, ..]) => *outer,
        }
    }

    /// The token of `e` itself, inside any parentheses.
    fn token(&self, e: &Expr) -> Span {
        let first = |len: usize| Span {
            end: e.span.start + len,
            ..e.span
        };
        match &e.kind {
            ExprKind::List(_) | ExprKind::Map(_) | ExprKind::Unary { .. } => first(1),
            ExprKind::Path(parts) => match parts.first() {
                Some(PathPart::Segment(s)) => first(1 + s.len()),
                _ => first(1),
            },
            ExprKind::Binary { op_span, .. } => *op_span,
            ExprKind::Ternary { question, .. } => *question,
            ExprKind::Is { keyword, .. } => *keyword,
            ExprKind::Call { callee, .. } => match callee {
                Callee::Global(name)
                | Callee::Namespaced { name, .. }
                | Callee::Method { name, .. } => name.span,
            },
            ExprKind::Member { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::Slice { object, .. } => self.first_token(object),
            _ => e.span,
        }
    }

    /// The first token of `e`, parentheses included.
    fn first_token(&self, e: &Expr) -> Span {
        if let Some(outer) = self.parens(e).first() {
            return *outer;
        }
        match &e.kind {
            ExprKind::Binary { left, .. } => self.first_token(left),
            ExprKind::Ternary { condition, .. } => self.first_token(condition),
            ExprKind::Is { value, .. } => self.first_token(value),
            ExprKind::Call {
                callee: Callee::Namespaced { namespace, .. },
                ..
            } => namespace.span,
            ExprKind::Call {
                callee: Callee::Method { receiver, .. },
                ..
            } => self.first_token(receiver),
            _ => self.token(e),
        }
    }

    /// A pattern literal that does not compile (RE2 syntax).
    fn regex(&mut self, args: &[Expr]) {
        if let Some(Expr {
            kind: ExprKind::String(pattern),
            span,
        }) = args.first()
            && (repeated_repetition(pattern) || regex::Regex::new(pattern).is_err())
        {
            self.diags.push(Diag::error(
                *span,
                format!("Invalid regular expression pattern. Pattern: {pattern}."),
            ));
        }
    }

    /// Recursion (a function calling itself, or a cycle of calls) and calls nested deeper than
    /// the compiler allows, found depth first from the functions in source order.
    fn cycles(&mut self) {
        let mut order: Vec<usize> = (0..self.functions.len()).collect();
        order.sort_by_key(|&i| self.functions[i].0.name.span.start);
        let mut state = vec![0u8; self.functions.len()]; // 0 new, 1 on the stack, 2 done
        let mut stack = Vec::new();
        // The calls, where to report them, and whether they are a cycle (or else too deep).
        let mut found: Vec<(Vec<usize>, usize, bool)> = Vec::new();
        for &start in &order {
            if state[start] == 0 {
                self.visit(start, &mut state, &mut stack, &mut found);
            }
        }
        for (calls, at, cycle) in found {
            let names: Vec<&str> = calls
                .iter()
                .map(|&i| self.functions[i].0.name.name.as_str())
                .collect();
            let message = if !cycle {
                format!(
                    "Maximum allowed call depth of {MAX_CALL_DEPTH} is reached for [{}] call \
                     stack.",
                    names.join("->")
                )
            } else if calls.len() == 1 {
                "Recursive call is not allowed.".to_owned()
            } else {
                format!(
                    "Detected a call cycle between [{}] functions.",
                    names.join(",")
                )
            };
            let span = self.functions[at].0.name.span;
            self.diags.push(Diag::error(span, message));
        }
    }

    fn visit(
        &self,
        f: usize,
        state: &mut [u8],
        stack: &mut Vec<usize>,
        found: &mut Vec<(Vec<usize>, usize, bool)>,
    ) {
        state[f] = 1;
        stack.push(f);
        let mut callees = self.calls[f].clone();
        callees.dedup();
        for g in callees {
            match state[g] {
                // One call more than the compiler allows: reported at the function called.
                0 if stack.len() > MAX_CALL_DEPTH => {
                    found.push((stack.clone(), g, false));
                    break;
                }
                0 => self.visit(g, state, stack, found),
                1 => {
                    let from = stack.iter().position(|&x| x == g).unwrap_or(0);
                    found.push((stack[from..].to_vec(), g, true));
                }
                _ => {}
            }
        }
        stack.pop();
        state[f] = 2;
    }
}

fn builtin(table: &'static [Builtin], name: &str) -> Option<&'static Builtin> {
    table.iter().find(|b| b.name == name)
}

/// A variable's type after one more use requiring `ty`: what both allow, or any type again
/// when nothing is left.
fn fold(current: Ty, ty: Ty) -> Ty {
    match (current, ty) {
        (current, None) => current,
        (None, ty) => ty,
        (Some(current), Some(ty)) => Some(current.filter(ty)).filter(|t| !t.is_empty()),
    }
}

/// Whether a repetition operator repeats another (`a**`, `a++`, `a*{2}`), which RE2 refuses
/// and Rust's `regex` accepts. One `?` after an operator makes it lazy and is fine.
pub(crate) fn repeated_repetition(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let mut after_repetition = false;
    let mut lazy = false;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 2;
            after_repetition = false;
            continue;
        }
        if in_class {
            in_class = c != ']';
            i += 1;
            continue;
        }
        let repetition = match c {
            '*' | '+' | '?' => Some(1),
            '{' => counted_repetition(&chars[i..]),
            _ => None,
        };
        match repetition {
            Some(_) if after_repetition && c == '?' && lazy => {
                lazy = false;
                i += 1;
            }
            Some(_) if after_repetition => return true,
            Some(len) => {
                after_repetition = true;
                lazy = true;
                i += len;
            }
            None => {
                after_repetition = false;
                lazy = false;
                i += 1;
                // `[]a]` and `[^]a]`: a bracket right after the opening one is literal.
                if c == '[' {
                    in_class = true;
                    if chars.get(i) == Some(&'^') {
                        i += 1;
                    }
                    if chars.get(i) == Some(&']') {
                        i += 1;
                    }
                }
            }
        }
    }
    false
}

/// The length of `{n}`, `{n,}` or `{n,m}` at the start of `chars`.
fn counted_repetition(chars: &[char]) -> Option<usize> {
    let close = chars.iter().position(|&c| c == '}')?;
    let inner: String = chars[1..close].iter().collect();
    let (min, max) = inner.split_once(',').unwrap_or((&inner, "0"));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    (!min.is_empty() && digits(min) && digits(max)).then_some(close + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repetitions_of_repetitions() {
        for bad in ["a**", "a++", "a*+", "a*??", "a{2}*", "a+{2}"] {
            assert!(repeated_repetition(bad), "{bad}");
        }
        for good in [
            "a*", "a*?", "a??", "a+?b+", "[*+]+", r"\*+", "a{2,3}?", "(a*)*", "a{x}*", "[]*]+",
        ] {
            assert!(!repeated_repetition(good), "{good}");
        }
    }

    #[test]
    fn comparability() {
        assert!(INT.comparable(FLOAT));
        assert!(NUMBER.comparable(FLOAT_INT));
        assert!(Types::of(&[T::String, T::Path]).comparable(PATH));
        assert!(!STRING.comparable(Types::of(&[T::Null])));
        assert!(!LIST.comparable(SET));
    }

    #[test]
    fn operand_types() {
        assert_eq!(
            result_of(ADD).list(),
            "int, float, string, duration, timestamp"
        );
        assert_eq!(
            operand_of(ADD, 0, Some(TIMESTAMP)).list(),
            "duration, timestamp"
        );
        assert_eq!(
            operand_of(ADD, 1, Some(TIMESTAMP)).list(),
            "timestamp, duration"
        );
        assert_eq!(
            operand_of(SUB, 0, Some(DURATION)).list(),
            "duration, timestamp"
        );
        assert_eq!(operand_of(NEG, 0, Some(INT)).list(), "int, float");
    }
}
