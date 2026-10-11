//! The checks after parsing, in the official compiler's stages:
//!
//! 1. [`structure`]: the version, imports and `match` paths (recursive wildcards, repeated
//!    variables). An unsupported version stops here.
//! 2. [`packages`]: a namespace (`math`, `duration`, …) used as a variable; [`complexity`]:
//!    expressions nested too deep, too many `let`s.
//! 3. [`semantics`], only when nothing so far was an error, and only for the first file:
//!    names (functions, variables, methods, services), arities, unused declarations, recursion,
//!    regular expression literals, `let` in version 1, comparisons that can never be true, and
//!    the limits on parameters, call depth and `match` nesting, paths and variables.
//!
//! The comparison check uses a small type inference over literals, wildcards (strings),
//! `null`, the namespaced constructors and the methods' results; everything else is untyped,
//! so it warns less often than the official compiler (#60 adds the full type checker).

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

/// Global functions and their arity.
const GLOBALS: &[(&str, usize)] = &[
    ("get", 1),
    ("exists", 1),
    ("getAfter", 1),
    ("existsAfter", 1),
    ("debug", 1),
    ("int", 1),
    ("float", 1),
    ("string", 1),
    ("path", 1),
    ("value", 1),
    ("vector", 1),
];

/// Functions in a namespace and their arity.
const NAMESPACED: &[(&str, usize)] = &[
    ("math.abs", 1),
    ("math.ceil", 1),
    ("math.floor", 1),
    ("math.isNaN", 1),
    ("math.pow", 2),
    ("math.round", 1),
    ("math.sqrt", 1),
    ("duration.abs", 1),
    ("duration.time", 4),
    ("duration.value", 2),
    ("timestamp.date", 3),
    ("timestamp.value", 1),
    ("latlng.value", 2),
    ("hashing.crc32", 1),
    ("hashing.crc32c", 1),
    ("hashing.md5", 1),
    ("hashing.sha256", 1),
];

/// Methods of any type and their arity: the compiler checks the name, not the receiver.
const METHODS: &[(&str, usize)] = &[
    ("get", 2),
    ("size", 0),
    ("matches", 1),
    ("keys", 0),
    ("hasAll", 1),
    ("toSet", 0),
    ("date", 0),
    ("time", 0),
    ("lower", 0),
    ("upper", 0),
    ("split", 1),
    ("replace", 2),
    ("trim", 0),
    ("join", 1),
    ("concat", 1),
    ("toMillis", 0),
    ("year", 0),
    ("month", 0),
    ("day", 0),
    ("hours", 0),
    ("minutes", 0),
    ("seconds", 0),
    ("nanos", 0),
    ("dayOfWeek", 0),
    ("dayOfYear", 0),
    ("distance", 1),
    ("latitude", 0),
    ("longitude", 0),
    ("bind", 1),
    ("toBase64", 0),
    ("toHexString", 0),
    ("toUtf8", 0),
    ("diff", 1),
    ("addedKeys", 0),
    ("affectedKeys", 0),
    ("changedKeys", 0),
    ("removedKeys", 0),
    ("unchangedKeys", 0),
    ("values", 0),
    ("difference", 1),
    ("intersection", 1),
    ("union", 1),
    ("removeAll", 1),
    ("hasAny", 1),
    ("hasOnly", 1),
];

/// A static type, as far as this check knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Any,
    Null,
    Bool,
    Int,
    Float,
    /// `math.abs`: an int or a float.
    Number,
    String,
    Bytes,
    List,
    Map,
    Set,
    Path,
    /// A recursive wildcard: its value is a string or a path.
    StringOrPath,
    Duration,
    Timestamp,
    LatLng,
    Vector,
}

impl Ty {
    fn comparable(self, other: Self) -> bool {
        use Ty::*;
        let numeric = |t| matches!(t, Int | Float | Number);
        self == Any
            || other == Any
            || self == other
            || (numeric(self) && numeric(other))
            || matches!(
                (self, other),
                (StringOrPath, String | Path) | (String | Path, StringOrPath)
            )
    }
}

fn global_type(name: &str) -> Ty {
    match name {
        "int" => Ty::Int,
        "float" => Ty::Float,
        "string" => Ty::String,
        "path" => Ty::Path,
        "vector" => Ty::Vector,
        _ => Ty::Any,
    }
}

fn namespaced_type(name: &str) -> Ty {
    match name {
        "math.isNaN" => Ty::Bool,
        n if n.starts_with("math.") => Ty::Number,
        n if n.starts_with("duration.") => Ty::Duration,
        n if n.starts_with("timestamp.") => Ty::Timestamp,
        n if n.starts_with("latlng.") => Ty::LatLng,
        n if n.starts_with("hashing.") => Ty::Bytes,
        _ => Ty::Any,
    }
}

fn method_type(name: &str) -> Ty {
    match name {
        "size" | "toMillis" | "year" | "month" | "day" | "hours" | "minutes" | "seconds"
        | "nanos" | "dayOfWeek" | "dayOfYear" => Ty::Int,
        "latitude" | "longitude" | "distance" => Ty::Float,
        "lower" | "upper" | "trim" | "toBase64" | "toHexString" => Ty::String,
        "toUtf8" => Ty::Bytes,
        "keys" | "values" => Ty::List,
        "toSet" | "addedKeys" | "affectedKeys" | "changedKeys" | "removedKeys"
        | "unchangedKeys" => Ty::Set,
        "matches" => Ty::Bool,
        "date" => Ty::Timestamp,
        "time" => Ty::Duration,
        "bind" => Ty::Path,
        _ => Ty::Any,
    }
}

/// A scope of function declarations: the top level, a service or a `match`.
struct Scope<'a> {
    parent: Option<usize>,
    /// Indexes into `Semantics::functions`, in source order.
    functions: Vec<usize>,
    /// The variables of the `match` path, if this is one.
    wildcards: Vec<&'a Segment>,
}

struct Semantics<'a> {
    diags: Vec<Diag>,
    scopes: Vec<Scope<'a>>,
    /// Every function and the scope it was declared in.
    functions: Vec<(&'a Function, usize)>,
    used: Vec<bool>,
    calls: Vec<Vec<usize>>,
    /// The scope of every `match`, in the order `declare_body` met them, and how many of them
    /// `body` has visited again.
    match_scopes: Vec<usize>,
    visited: usize,
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

/// A parameter or `let` while its function is checked.
struct Local {
    name: String,
    ty: Ty,
    used: bool,
}

pub(crate) fn semantics(file: &FileAst, version: Version, diags: &mut Vec<Diag>) {
    let mut s = Semantics {
        diags: Vec::new(),
        scopes: vec![Scope {
            parent: None,
            functions: Vec::new(),
            wildcards: Vec::new(),
        }],
        functions: Vec::new(),
        used: Vec::new(),
        calls: Vec::new(),
        match_scopes: Vec::new(),
        visited: 0,
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

    let mut lets = Vec::new();
    for i in 0..s.functions.len() {
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
        self.scopes.push(Scope {
            parent: Some(parent),
            functions: Vec::new(),
            wildcards,
        });
        self.scopes.len() - 1
    }

    fn declare(&mut self, f: &'a Function, scope: usize) {
        self.scopes[scope].functions.push(self.functions.len());
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
        for scope in &self.scopes {
            let mut names: Vec<&str> = Vec::new();
            for &i in &scope.functions {
                let name = &self.functions[i].0.name;
                if names.contains(&name.name.as_str()) {
                    self.diags.push(Diag::error(
                        name.span,
                        format!("Function {} is already defined.", name.name),
                    ));
                    self.used[i] = true;
                }
                names.push(&name.name);
            }
        }
    }

    /// The function `name` as seen from `scope`.
    fn resolve_function(&self, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            let found = self.scopes[scope]
                .functions
                .iter()
                .copied()
                .find(|&i| self.functions[i].0.name.name == name);
            if found.is_some() {
                return found;
            }
            scope = self.scopes[scope].parent?;
        }
    }

    /// A wildcard's type: a string, and a recursive one a path, or a string or a path when
    /// its `match` path also has a single-segment wildcard (as measured).
    fn resolve_wildcard(&self, mut scope: usize, name: &str) -> Option<Ty> {
        loop {
            let wildcards = &self.scopes[scope].wildcards;
            let found = wildcards
                .iter()
                .rev()
                .find(|s| s.kind.variable() == Some(name));
            if let Some(segment) = found {
                let single = wildcards
                    .iter()
                    .any(|s| matches!(s.kind, SegmentKind::Wildcard(_)));
                return Some(match segment.kind {
                    SegmentKind::Recursive(_) if single => Ty::StringOrPath,
                    SegmentKind::Recursive(_) => Ty::Path,
                    _ => Ty::String,
                });
            }
            scope = self.scopes[scope].parent?;
        }
    }

    /// Checks function `i`; the names of its `let`s go to `lets`.
    fn function(&mut self, i: usize, lets: &mut Vec<Span>) {
        let (f, scope) = self.functions[i];
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
        let mut locals: Vec<Local> = Vec::new();
        let mut spans = Vec::new();
        for p in &f.params {
            // A repeated parameter is an error; neither is reported unused.
            if let Some(first) = locals.iter_mut().find(|l| l.name == p.name) {
                first.used = true;
                self.diags.push(Diag::error(
                    p.span,
                    format!("Variable {} is already defined.", p.name),
                ));
            }
            locals.push(Local {
                name: p.name.clone(),
                ty: Ty::Any,
                used: false,
            });
            spans.push(p.span);
        }
        for l in &f.lets {
            lets.push(l.name.span);
            let ty = self.expr(&l.value, scope, &mut locals, Some(i));
            if locals.iter().any(|local| local.name == l.name.name) {
                self.diags.push(Diag::error(
                    l.name.span,
                    format!(
                        "The identifier [{}] is already bound in the current scope.",
                        l.name.name
                    ),
                ));
            }
            locals.push(Local {
                name: l.name.name.clone(),
                ty,
                used: false,
            });
            spans.push(l.name.span);
        }
        self.expr(&f.result, scope, &mut locals, Some(i));
        for (local, span) in locals.iter().zip(spans) {
            if !local.used {
                self.diags.push(Diag::warning(
                    span,
                    format!("Unused variable: {}.", local.name),
                ));
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
                self.expr(c, scope, &mut Vec::new(), None);
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

    /// Checks `e` and returns its type. `caller` is the function being checked, if any.
    fn expr(
        &mut self,
        e: &Expr,
        scope: usize,
        locals: &mut Vec<Local>,
        caller: Option<usize>,
    ) -> Ty {
        match &e.kind {
            ExprKind::Null => Ty::Null,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Int(_) | ExprKind::IntOverflow(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::String(_) => Ty::String,
            ExprKind::Bytes(_) => Ty::Bytes,
            ExprKind::Ident(name) => {
                if let Some(local) = locals.iter_mut().rev().find(|l| &l.name == name) {
                    local.used = true;
                    return local.ty;
                }
                if let Some(ty) = self.resolve_wildcard(scope, name) {
                    return ty;
                }
                if !matches!(name.as_str(), "request" | "resource") && !is_package(name) {
                    self.diags.push(Diag::warning(
                        e.span,
                        format!("Invalid variable name: {name}."),
                    ));
                }
                Ty::Any
            }
            ExprKind::List(items) => {
                for item in items {
                    self.expr(item, scope, locals, caller);
                }
                Ty::List
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    self.expr(k, scope, locals, caller);
                    self.expr(v, scope, locals, caller);
                }
                Ty::Map
            }
            ExprKind::Path(parts) => {
                for part in parts {
                    if let PathPart::Expr(e) = part {
                        self.expr(e, scope, locals, caller);
                    }
                }
                Ty::Path
            }
            ExprKind::Member { object, .. } => {
                self.expr(object, scope, locals, caller);
                Ty::Any
            }
            ExprKind::Index { object, index } => {
                self.expr(object, scope, locals, caller);
                self.expr(index, scope, locals, caller);
                Ty::Any
            }
            ExprKind::Slice { object, start, end } => {
                self.expr(object, scope, locals, caller);
                self.expr(start, scope, locals, caller);
                self.expr(end, scope, locals, caller);
                Ty::Any
            }
            ExprKind::Call { callee, args } => {
                for a in args {
                    self.expr(a, scope, locals, caller);
                }
                self.call(callee, args, scope, locals, caller)
            }
            ExprKind::Unary { op, operand } => {
                let ty = self.expr(operand, scope, locals, caller);
                match op {
                    UnaryOp::Not => Ty::Bool,
                    UnaryOp::Neg if matches!(ty, Ty::Int | Ty::Float | Ty::Number) => ty,
                    UnaryOp::Neg => Ty::Any,
                }
            }
            ExprKind::Binary {
                op,
                op_span,
                left,
                right,
            } => {
                let l = self.expr(left, scope, locals, caller);
                let r = self.expr(right, scope, locals, caller);
                match op {
                    BinaryOp::Eq | BinaryOp::Ne => {
                        if !l.comparable(r) {
                            let result = if *op == BinaryOp::Eq { "false" } else { "true" };
                            self.diags.push(Diag::warning(
                                *op_span,
                                format!(
                                    "The sub-expressions are not comparable, so this \
                                     comparison will always return {result}."
                                ),
                            ));
                        }
                        Ty::Bool
                    }
                    BinaryOp::Or
                    | BinaryOp::And
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::In => Ty::Bool,
                    BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Mod => Ty::Any,
                }
            }
            ExprKind::Is { value, .. } => {
                self.expr(value, scope, locals, caller);
                Ty::Bool
            }
            ExprKind::Ternary {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, scope, locals, caller);
                self.expr(then, scope, locals, caller);
                self.expr(otherwise, scope, locals, caller);
                Ty::Any
            }
        }
    }

    fn call(
        &mut self,
        callee: &Callee,
        args: &[Expr],
        scope: usize,
        locals: &mut Vec<Local>,
        caller: Option<usize>,
    ) -> Ty {
        match callee {
            Callee::Global(name) => {
                if let Some(f) = self.resolve_function(scope, &name.name) {
                    self.used[f] = true;
                    if let Some(caller) = caller {
                        self.calls[caller].push(f);
                    }
                    if self.functions[f].0.params.len() != args.len() {
                        self.arity(name, &name.name);
                    }
                    return Ty::Any;
                }
                match GLOBALS.iter().find(|(n, _)| *n == name.name) {
                    Some(&(_, arity)) => {
                        if arity != args.len() {
                            self.arity(name, &name.name);
                        }
                        global_type(&name.name)
                    }
                    None => {
                        self.unknown_function(name, &name.name);
                        Ty::Any
                    }
                }
            }
            Callee::Namespaced { namespace, name } => {
                let full = format!("{}.{}", namespace.name, name.name);
                match NAMESPACED.iter().find(|(n, _)| *n == full) {
                    Some(&(_, arity)) => {
                        if arity != args.len() {
                            self.arity(name, &full);
                        }
                        namespaced_type(&full)
                    }
                    None => {
                        self.unknown_function(name, &full);
                        Ty::Any
                    }
                }
            }
            Callee::Method { receiver, name } => {
                self.expr(receiver, scope, locals, caller);
                match METHODS.iter().find(|(n, _)| *n == name.name) {
                    Some(&(_, arity)) => {
                        if arity != args.len() {
                            self.arity(name, &name.name);
                        }
                        if matches!(name.name.as_str(), "matches" | "split") {
                            self.regex(args);
                        }
                        method_type(&name.name)
                    }
                    None => {
                        self.unknown_function(name, &name.name);
                        Ty::Any
                    }
                }
            }
        }
    }

    fn arity(&mut self, at: &Ident, name: &str) {
        self.diags.push(Diag::warning(
            at.span,
            format!("Incorrect number of arguments supplied to function: {name}."),
        ));
    }

    fn unknown_function(&mut self, at: &Ident, name: &str) {
        self.diags.push(Diag::warning(
            at.span,
            format!("Invalid function name: {name}."),
        ));
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

/// Whether a repetition operator repeats another (`a**`, `a++`, `a*{2}`), which RE2 refuses
/// and Rust's `regex` accepts. One `?` after an operator makes it lazy and is fine.
fn repeated_repetition(pattern: &str) -> bool {
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
        assert!(Ty::Int.comparable(Ty::Float));
        assert!(Ty::Any.comparable(Ty::Null));
        assert!(Ty::StringOrPath.comparable(Ty::Path));
        assert!(!Ty::String.comparable(Ty::Null));
        assert!(!Ty::LatLng.comparable(Ty::Null));
    }
}
