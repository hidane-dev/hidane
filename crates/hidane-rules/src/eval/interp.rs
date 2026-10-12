//! The tree-walking interpreter: variables, functions, operators and lookups.

use std::{cell::OnceCell, collections::HashMap, rc::Rc};

use indexmap::IndexMap;

use super::{
    Budget, Documents, constraint,
    library::{self, to_string},
    trace::{NodeKey, Trace, observe},
    value::{
        Error, R, Val, contains, eq, loose_eq, read, resource, strict_eq, utf16_cmp, utf16_len,
    },
};
use crate::{
    ast::{BinaryOp, Callee, Expr, ExprKind, Function, PathPart, Span, UnaryOp},
    coverage,
};

/// Where names resolve: level 0 is the top of the file, 1 the service, then each `match` from
/// the outermost to the one whose `allow` is evaluated.
pub(crate) struct Level<'a> {
    pub functions: &'a [Function],
    /// The variables the level binds: a `match`'s wildcards.
    pub variables: Vec<(&'a str, Val)>,
}

/// What a condition sees and the lookups it makes.
pub(crate) struct Interp<'a> {
    levels: Vec<Level<'a>>,
    request: Val,
    resource: Val,
    /// The path of the request: its document is free to look up, and its database is the
    /// only one `get()` may read.
    target: &'a [String],
    documents: &'a dyn Documents,
    budget: &'a mut Budget,
    /// What `debug()` received and, when traced, what each expression evaluated to.
    trace: &'a mut Trace,
    /// The ruleset's parenthesized expressions (`Ruleset::parens`).
    parens: &'a HashMap<(usize, usize), Vec<Span>>,
    /// Documents looked up by this evaluation, before (`false`) or after (`true`) the writes.
    cache: HashMap<(bool, Vec<String>), Option<Val>>,
    /// Compiled patterns, by whether they must match the whole string and their text.
    regexes: HashMap<(bool, String), Option<Rc<regex::Regex>>>,
    depth: usize,
    calls: usize,
}

/// The parameters and `let`s of a function call (none for an `allow` condition).
pub(crate) struct Frame<'a> {
    level: usize,
    locals: Vec<Local<'a>>,
}

struct Local<'a> {
    name: &'a str,
    /// A `let`'s expression, evaluated in order before the function's result.
    expr: Option<&'a Expr>,
    value: OnceCell<R>,
}

/// A frame and how many of its locals are in scope: a `let` sees the ones before it.
#[derive(Clone, Copy)]
pub(crate) struct Scope<'f, 'a> {
    frame: &'f Frame<'a>,
    visible: usize,
}

/// How deep expressions and calls may nest while evaluating. The compiler bounds both, but not
/// their product; this keeps the stack bounded (a few hundred bytes a level in a release build).
const MAX_DEPTH: usize = 2_500;
const MAX_CALL_DEPTH: usize = 20;

/// The step that evaluates one kind of expression.
type Step<'a> = for<'f> fn(&mut Interp<'a>, &'a Expr, Scope<'f, 'a>) -> R;

impl<'a> Frame<'a> {
    /// The frame of an `allow` condition at `level`.
    pub fn condition(level: usize) -> Self {
        Self {
            level,
            locals: Vec::new(),
        }
    }

    pub fn scope(&self) -> Scope<'_, 'a> {
        Scope {
            frame: self,
            visible: self.locals.len(),
        }
    }
}

impl<'a> Interp<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        levels: Vec<Level<'a>>,
        request: Val,
        resource: Val,
        target: &'a [String],
        documents: &'a dyn Documents,
        budget: &'a mut Budget,
        trace: &'a mut Trace,
        parens: &'a HashMap<(usize, usize), Vec<Span>>,
    ) -> Self {
        Self {
            levels,
            request,
            resource,
            target,
            documents,
            budget,
            trace,
            parens,
            cache: HashMap::new(),
            regexes: HashMap::new(),
            depth: 0,
            calls: 0,
        }
    }

    /// Evaluates `e`. Each level of nesting goes through here and one small function for its
    /// kind of expression, which keeps the stack a recursion uses small.
    pub fn eval(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        if self.depth >= MAX_DEPTH {
            return Err(too_complex());
        }
        self.depth += 1;
        // One call through a pointer: in a debug build each call of a `match` would keep a
        // slot of its own in this frame.
        let step: Step<'a> = match &e.kind {
            ExprKind::Ident(_) => Self::variable_of,
            ExprKind::List(_) => Self::list,
            ExprKind::Map(_) => Self::map,
            ExprKind::Path(_) => Self::path_of,
            ExprKind::Member { .. } => Self::member,
            ExprKind::Index { .. } => Self::index,
            ExprKind::Slice { .. } => Self::slice,
            ExprKind::Call { .. } => Self::call_of,
            ExprKind::Unary { .. } => Self::unary,
            ExprKind::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            } => Self::logical,
            ExprKind::Binary { .. } => Self::binary,
            ExprKind::Is { .. } => Self::is,
            ExprKind::Ternary { .. } => Self::ternary,
            _ => |_, e, _| literal(e),
        };
        let result = step(self, e, s);
        self.depth -= 1;
        if result.is_err() || self.trace.tracing() {
            return self.finish(e, result);
        }
        result
    }

    /// Places an error `e` raised that its step did not place (a member, an index, a method
    /// that does not exist, `in`, a variable) at its first token, and traces the result.
    #[inline(never)]
    fn finish(&mut self, e: &Expr, result: R) -> R {
        let result = result.map_err(|err| err.at(self.start(e)));
        if self.trace.tracing() && coverage::is_node(e) {
            self.trace.record(NodeKey::of(e), &result);
        }
        result
    }

    /// Where the coverage report starts `e`: its first token, or the outermost parenthesis
    /// before it.
    fn start(&self, e: &Expr) -> usize {
        coverage::start(self.parens, e).start
    }

    /// Reports `e` and every expression in it as not evaluated.
    fn skip(&mut self, e: &Expr) {
        if self.trace.tracing() {
            coverage::walk(e, &mut |node| self.trace.skip(NodeKey::of(node)));
        }
    }

    /// `debug(value)`.
    pub fn debug(&mut self, value: &Val) {
        self.trace.debug.push(observe(value));
    }

    fn variable_of(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Ident(name) = &e.kind else {
            unreachable!()
        };
        self.variable(name, s)
    }

    fn path_of(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Path(parts) = &e.kind else {
            unreachable!()
        };
        self.path(parts, s)
    }

    fn call_of(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Call { callee, args } = &e.kind else {
            unreachable!()
        };
        self.call(callee, args, s)
    }

    /// The operands of an operator, a literal or a call, as the official emulator evaluates
    /// them: all of them, the last first (as its `debug()` output shows), and the first error in
    /// order wins.
    fn operands(&mut self, exprs: &[&'a Expr], s: Scope<'_, 'a>) -> Result<Vec<Val>, Error> {
        let mut results: Vec<R> = Vec::with_capacity(exprs.len());
        for e in exprs.iter().rev() {
            results.push(self.eval(e, s));
        }
        results.into_iter().rev().collect()
    }

    fn list(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::List(items) = &e.kind else {
            unreachable!()
        };
        let items: Vec<&Expr> = items.iter().collect();
        Ok(Val::list(self.operands(&items, s)?))
    }

    /// A map literal: keys must be strings, and a repeated key keeps the last value.
    fn map(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Map(entries) = &e.kind else {
            unreachable!()
        };
        let parts: Vec<&Expr> = entries.iter().flat_map(|(k, v)| [k, v]).collect();
        let values = self.operands(&parts, s)?;
        let mut out = IndexMap::with_capacity(entries.len());
        for ((k, _), pair) in entries.iter().zip(values.chunks(2)) {
            let [key, value] = pair else {
                unreachable!("a key and a value")
            };
            let Val::Str(key) = key else {
                return Err(Error::type_error(key.type_name(), "string").at(self.start(k)));
            };
            out.insert(key.to_string(), value.clone());
        }
        Ok(Val::Map(Rc::new(out)))
    }

    fn member(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Member { object, field } = &e.kind else {
            unreachable!()
        };
        let object = self.eval(object, s)?;
        member(&object, &field.name)
    }

    fn index(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Index { object, index } = &e.kind else {
            unreachable!()
        };
        let [object, index] = self.fixed([object, index], s)?;
        library::index(&object, &index)
    }

    fn slice(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Slice { object, start, end } = &e.kind else {
            unreachable!()
        };
        let [object, start, end] = self.fixed([object, start, end], s)?;
        library::slice(&object, &start, &end)
    }

    /// [`Interp::operands`] of a fixed number.
    fn fixed<const N: usize>(
        &mut self,
        exprs: [&'a Expr; N],
        s: Scope<'_, 'a>,
    ) -> Result<[Val; N], Error> {
        let values = self.operands(&exprs, s)?;
        Ok(values
            .try_into()
            .unwrap_or_else(|_| unreachable!("one value each")))
    }

    fn unary(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Unary { op, operand } = &e.kind else {
            unreachable!()
        };
        let result = match op {
            UnaryOp::Not => match self.eval(operand, s)? {
                Val::Bool(b) => Ok(Val::Bool(!b)),
                other => Err(not(&other)),
            },
            UnaryOp::Neg => {
                // `-9223372036854775808` is the smallest int, not the negation of a literal
                // that does not fit.
                if let ExprKind::IntOverflow(text) = &operand.kind
                    && text.parse::<i128>().ok() == Some(-i128::from(i64::MIN))
                {
                    return Ok(Val::Int(i64::MIN));
                }
                negate(self.eval(operand, s)?)
            }
        };
        // At the operator.
        result.map_err(|err| err.at(e.span.start))
    }

    fn binary(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Binary {
            op,
            op_span,
            left,
            right,
        } = &e.kind
        else {
            unreachable!()
        };
        // `x in y` is `y`'s method: `y` comes first.
        let [l, r] = if *op == BinaryOp::In {
            let [r, l] = self.fixed([right, left], s)?;
            [l, r]
        } else {
            self.fixed([left, right], s)?
        };
        // At the operator, but `in` at the expression's start.
        binary(*op, &l, &r).map_err(|err| match op {
            BinaryOp::In => err,
            _ => err.at(op_span.start),
        })
    }

    fn is(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Is { value, type_name } = &e.kind else {
            unreachable!()
        };
        let v = self.eval(value, s)?;
        Ok(Val::Bool(v.is(&type_name.name)))
    }

    fn ternary(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Ternary {
            condition,
            question,
            then,
            otherwise,
        } = &e.kind
        else {
            unreachable!()
        };
        match self.eval(condition, s)? {
            Val::Bool(true) => {
                let result = self.eval(then, s);
                self.skip(otherwise);
                result
            }
            Val::Bool(false) => {
                self.skip(then);
                self.eval(otherwise, s)
            }
            other => Err(Error::type_error(other.type_name(), "bool").at(question.start)),
        }
    }

    /// `&&` and `||` absorb an error when the other side decides, as CEL does: `false && error`
    /// and `error && false` are false, `true || error` and `error || true` are true. A value
    /// that is not a bool is an error like any other: on the left at the operator, on the right
    /// at the expression's start.
    fn logical(&mut self, e: &'a Expr, s: Scope<'_, 'a>) -> R {
        let ExprKind::Binary {
            op,
            op_span,
            left,
            right,
        } = &e.kind
        else {
            unreachable!()
        };
        let and = *op == BinaryOp::And;
        // `and` is the value that leaves the result to the other side.
        let error = match self.eval(left, s) {
            Ok(Val::Bool(l)) if l != and => {
                self.skip(right);
                return Ok(Val::Bool(l));
            }
            Ok(Val::Bool(_)) => return self.eval(right, s).and_then(boolean).map(Val::Bool),
            Ok(other) => not_bool(&other).at(op_span.start),
            Err(e) => e,
        };
        match self.eval(right, s) {
            Ok(Val::Bool(r)) if r != and => Ok(Val::Bool(r)),
            _ => Err(error),
        }
    }

    fn variable(&mut self, name: &str, s: Scope<'_, 'a>) -> R {
        let locals = &s.frame.locals[..s.visible];
        if let Some(i) = locals.iter().rposition(|l| l.name == name) {
            let local = &locals[i];
            if let Some(v) = local.value.get() {
                return v.clone();
            }
            let expr = local
                .expr
                .expect("parameters are bound when the call starts");
            let v = self.eval(
                expr,
                Scope {
                    frame: s.frame,
                    visible: i,
                },
            );
            return local.value.get_or_init(|| v).clone();
        }
        for level in self.levels[..=s.frame.level].iter().rev() {
            if let Some((_, v)) = level.variables.iter().rev().find(|(n, _)| *n == name) {
                return Ok(v.clone());
            }
        }
        match name {
            "request" => Ok(self.request.clone()),
            "resource" => Ok(self.resource.clone()),
            // An unknown name (the compiler warned about it) has no value.
            _ => Err(Error::null()),
        }
    }

    /// The function `name` as seen from `level`, and the level it was declared at.
    fn function(&self, level: usize, name: &str) -> Option<(&'a Function, usize)> {
        (0..=level).rev().find_map(|l| {
            self.levels[l]
                .functions
                .iter()
                .find(|f| f.name.name == name)
                .map(|f| (f, l))
        })
    }

    /// A call. Its errors are at the function's name, but a method the value does not have is
    /// at the expression's start.
    fn call(&mut self, callee: &'a Callee, args: &'a [Expr], s: Scope<'_, 'a>) -> R {
        match callee {
            // A function that does not exist fails before its arguments are evaluated.
            Callee::Global(name) => {
                let at = |err: Error| err.at(name.span.start);
                if let Some((f, level)) = self.function(s.frame.level, &name.name) {
                    return self.call_function(f, level, args, s).map_err(at);
                }
                if !library::is_global(&name.name) {
                    return Err(at(Error::not_found(&name.name)));
                }
                let args = self.operands(&args.iter().collect::<Vec<_>>(), s)?;
                library::global(self, &name.name, args).map_err(at)
            }
            Callee::Namespaced { namespace, name } => {
                let at = |err: Error| err.at(name.span.start);
                let full = format!("{}.{}", namespace.name, name.name);
                if !library::is_namespaced(&full) {
                    return Err(at(Error::not_found(&full)));
                }
                let args = self.operands(&args.iter().collect::<Vec<_>>(), s)?;
                library::namespaced(&full, args).map_err(at)
            }
            // The arguments are evaluated before the receiver, but a null receiver or a method
            // it does not have fails before their errors count.
            Callee::Method { receiver, name } => {
                let at = |err: Error| err.at(name.span.start);
                let values: Vec<R> = args.iter().rev().map(|a| self.eval(a, s)).collect();
                let receiver = self.eval(receiver, s)?;
                if let Val::Null = receiver {
                    return Err(at(Error::null()));
                }
                if !library::has_method(receiver.type_name(), &name.name) {
                    return Err(Error::not_found(&name.name));
                }
                let args = values.into_iter().rev().collect::<Result<Vec<_>, _>>()?;
                library::method(self, &receiver, &name.name, args).map_err(at)
            }
        }
    }

    /// A call of a function of the ruleset. Its arguments (the last first), then its `let`s in
    /// order, are evaluated before its result (the official emulator logs a `debug()` in a `let`
    /// that is not used), but an error in one only counts if the function uses it.
    fn call_function(
        &mut self,
        f: &'a Function,
        level: usize,
        args: &'a [Expr],
        s: Scope<'_, 'a>,
    ) -> R {
        if args.len() != f.params.len() {
            let params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
            return Err(Error::arity(
                args.len(),
                &[format!("{}({})", f.name.name, params.join(", "))],
            ));
        }
        if self.calls >= MAX_CALL_DEPTH {
            return Err(Error::new(format!(
                "Maximum allowed call depth of {MAX_CALL_DEPTH} is reached."
            )));
        }
        let mut values: Vec<R> = args.iter().rev().map(|arg| self.eval(arg, s)).collect();
        let mut locals = Vec::with_capacity(args.len() + f.lets.len());
        for param in &f.params {
            locals.push(Local {
                name: &param.name,
                expr: None,
                value: OnceCell::from(values.pop().expect("one value per parameter")),
            });
        }
        for l in &f.lets {
            locals.push(Local {
                name: &l.name.name,
                expr: Some(&l.value),
                value: OnceCell::new(),
            });
        }
        let frame = Frame { level, locals };
        self.calls += 1;
        for (i, local) in frame.locals.iter().enumerate().skip(f.params.len()) {
            if let Some(expr) = local.expr
                && local.value.get().is_none()
            {
                let v = self.eval(
                    expr,
                    Scope {
                        frame: &frame,
                        visible: i,
                    },
                );
                let _ = local.value.set(v);
            }
        }
        let result = self.eval(&f.result, frame.scope());
        self.calls -= 1;
        // The body from each `let` on, in the coverage report, has the function's result.
        for l in &f.lets {
            self.trace.record(NodeKey::Body(l.span.start), &result);
        }
        result
    }

    /// A path literal. Literal segments are URL-decoded; `$(…)` adds a path's segments, or one
    /// segment of anything `string()` converts.
    fn path(&mut self, parts: &'a [PathPart], s: Scope<'_, 'a>) -> R {
        let exprs: Vec<&Expr> = parts
            .iter()
            .filter_map(|part| match part {
                PathPart::Expr(e) => Some(e),
                PathPart::Segment(_) => None,
            })
            .collect();
        let mut values = self.operands(&exprs, s)?.into_iter().zip(exprs);
        let mut segments = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                PathPart::Segment(text) => segments.push(url_decode(text)),
                PathPart::Expr(_) => match values.next() {
                    Some((Val::Path(p), _)) => segments.extend(p.iter().cloned()),
                    Some((v, e)) => {
                        segments.push(to_string(&v).map_err(|err| err.at(self.start(e)))?);
                    }
                    None => unreachable!("one value per part"),
                },
            }
        }
        Ok(Val::path(segments))
    }

    /// A compiled regular expression, or the official error for one that does not compile.
    pub fn regex(&mut self, pattern: &str, whole: bool) -> Result<Rc<regex::Regex>, Error> {
        let compiled = self
            .regexes
            .entry((whole, pattern.to_owned()))
            .or_insert_with(|| {
                if crate::check::repeated_repetition(pattern) {
                    return None;
                }
                let rust = re2_to_rust(pattern);
                let rust = if whole { format!("^(?:{rust})$") } else { rust };
                regex::Regex::new(&rust).ok().map(Rc::new)
            });
        compiled.clone().ok_or_else(|| Error::regex(pattern))
    }

    /// `get()` and its kin: the document at `path` before (`after == false`) or after the
    /// request's writes, `None` if there is none. Each distinct document counts against the
    /// budget once, except the request's own and those its batch writes; a path that is not a
    /// document of this database is an error.
    pub fn lookup(
        &mut self,
        function: &str,
        path: &[String],
        after: bool,
    ) -> Result<Option<Val>, Error> {
        let document = path.len() >= 5
            && path.len() % 2 == 1
            && path[0] == "databases"
            && self.target.get(1) == Some(&path[1])
            && path[2] == "documents";
        if !document {
            return Err(Error::service(function, path));
        }
        if let Some(found) = self.cache.get(&(after, path.to_vec())) {
            return Ok(found.clone());
        }
        let budget = &mut *self.budget;
        let free = path == self.target || budget.written.iter().any(|p| p.as_slice() == path);
        if !free && !budget.looked_up.iter().any(|p| p.as_slice() == path) {
            if budget.this_part >= budget.part_limit || budget.looked_up.len() >= budget.total_limit
            {
                return Err(Error::service(function, path));
            }
            budget.looked_up.push(path.to_vec());
            budget.this_part += 1;
        }
        let found = if after {
            self.documents.get_after(path)
        } else {
            self.documents.get(path)
        }
        .map(|doc| resource(&doc));
        self.cache.insert((after, path.to_vec()), found.clone());
        Ok(found)
    }
}

#[cold]
fn too_complex() -> Error {
    Error::new("Expression is too complex to evaluate safely.".to_owned())
}

/// A literal: null, a bool, a number, a string or bytes.
#[inline(never)]
fn literal(e: &Expr) -> R {
    Ok(match &e.kind {
        ExprKind::Null => Val::Null,
        ExprKind::Bool(b) => Val::Bool(*b),
        ExprKind::Int(i) => Val::Int(*i),
        ExprKind::IntOverflow(_) => return Err(Error::overflow()),
        ExprKind::Float(f) => Val::Float(*f),
        ExprKind::String(text) => Val::str(text),
        ExprKind::Bytes(b) => Val::Bytes(Rc::from(b.as_slice())),
        _ => unreachable!("only literals come here"),
    })
}

#[cold]
fn not(v: &Val) -> Error {
    Error::unsupported(&format!("!{}", v.type_name()), &["!bool".to_owned()])
}

/// A condition's value as a bool.
pub(crate) fn boolean(v: Val) -> Result<bool, Error> {
    match v {
        Val::Bool(b) => Ok(b),
        other => Err(not_bool(&other)),
    }
}

#[cold]
fn not_bool(v: &Val) -> Error {
    Error::type_error(v.type_name(), "bool")
}

/// `object.field`.
#[inline(never)]
fn member(object: &Val, field: &str) -> R {
    match object {
        Val::Map(map) => read(map.get(field)).unwrap_or_else(|| Err(Error::property(field))),
        Val::Null => Err(Error::null()),
        // A path's fields are the variables bound into it, and literal paths bind none.
        Val::Path(_) => Err(Error::property(field)),
        other => Err(Error::type_error(other.type_name(), "map,path")),
    }
}

fn negate(v: Val) -> R {
    match v {
        Val::Int(i) => i.checked_neg().map(Val::Int).ok_or_else(Error::overflow),
        Val::Float(f) => Ok(Val::Float(-f)),
        Val::Duration(d) => Val::duration(-d),
        other => Err(Error::unsupported(
            &format!("-{}", other.type_name()),
            &["-int", "-float", "-duration"].map(String::from),
        )),
    }
}

fn binary(op: BinaryOp, l: &Val, r: &Val) -> R {
    if let Some(result) = constraint::binary(op, l, r) {
        return result;
    }
    match op {
        BinaryOp::Eq => Ok(Val::Bool(eq(l, r))),
        BinaryOp::Ne => Ok(Val::Bool(!eq(l, r))),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => compare(op, l, r),
        BinaryOp::In => member_of(l, r),
        BinaryOp::Add => add(l, r),
        BinaryOp::Sub => sub(l, r),
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => multiplicative(op, l, r),
        BinaryOp::And | BinaryOp::Or => unreachable!("logical operators are evaluated lazily"),
    }
}

fn symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::In => "in",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
    }
}

/// The official "Unsupported operation error" for a binary operator, listing what it takes.
fn unsupported(op: BinaryOp, l: &Val, r: &Val, expected: &[(&str, &str)]) -> Error {
    let op = symbol(op);
    Error::unsupported(
        &format!("{} {op} {}", l.type_name(), r.type_name()),
        &expected
            .iter()
            .map(|(a, b)| format!("{a} {op} {b}"))
            .collect::<Vec<_>>(),
    )
}

fn compare(op: BinaryOp, l: &Val, r: &Val) -> R {
    use std::cmp::Ordering;
    let ordering: Option<Ordering> = match (l, r) {
        (Val::Str(a), Val::Str(b)) => Some(utf16_cmp(a, b)),
        (Val::Bytes(a), Val::Bytes(b)) => Some(a.cmp(b)),
        (Val::Int(a), Val::Int(b)) => Some(a.cmp(b)),
        (Val::Float(a), Val::Float(b)) => a.partial_cmp(b),
        (Val::Int(a), Val::Float(b)) => (*a as f64).partial_cmp(b),
        (Val::Float(a), Val::Int(b)) => a.partial_cmp(&(*b as f64)),
        (Val::Duration(a), Val::Duration(b)) => Some(a.cmp(b)),
        (Val::Timestamp(s, n), Val::Timestamp(t, m)) => Some((s, n).cmp(&(t, m))),
        _ => {
            return Err(unsupported(
                op,
                l,
                r,
                &[
                    ("string", "string"),
                    ("bytes", "bytes"),
                    ("int", "int"),
                    ("float", "float"),
                    ("duration", "duration"),
                    ("timestamp", "timestamp"),
                    ("constraint", "any"),
                    ("any", "constraint"),
                ],
            ));
        }
    };
    // NaN compares false either way.
    Ok(Val::Bool(ordering.is_some_and(|o| match op {
        BinaryOp::Lt => o.is_lt(),
        BinaryOp::Le => o.is_le(),
        BinaryOp::Gt => o.is_gt(),
        _ => o.is_ge(),
    })))
}

/// `value in collection`: list items compare strictly, set items by value, map keys must be
/// strings.
fn member_of(v: &Val, collection: &Val) -> R {
    match collection {
        Val::List(items) => Ok(Val::Bool(contains(items, v, strict_eq))),
        Val::Set(items) => Ok(Val::Bool(contains(items, v, loose_eq))),
        Val::Map(map) => match v {
            Val::Str(key) => Ok(Val::Bool(map.contains_key(&**key))),
            other => Err(Error::unsupported(
                &format!("map.in({})", other.type_name()),
                &["map.in(string)".to_owned()],
            )),
        },
        Val::Null => Err(Error::null()),
        _ => Err(Error::not_found("in")),
    }
}

fn add(l: &Val, r: &Val) -> R {
    match (l, r) {
        (Val::Int(a), Val::Int(b)) => a.checked_add(*b).map(Val::Int).ok_or_else(Error::overflow),
        (Val::Str(a), Val::Str(b)) => Ok(Val::string(format!("{a}{b}"))),
        (Val::Duration(a), Val::Duration(b)) => Val::duration(a + b),
        (Val::Timestamp(s, n), Val::Duration(d)) | (Val::Duration(d), Val::Timestamp(s, n)) => {
            Val::timestamp_nanos(timestamp_nanos(*s, *n) + d)
        }
        _ => match (number(l), number(r)) {
            (Some(a), Some(b)) => Ok(Val::Float(a + b)),
            _ => Err(unsupported(
                BinaryOp::Add,
                l,
                r,
                &[
                    ("int", "int"),
                    ("float", "float"),
                    ("string", "string"),
                    ("duration", "duration"),
                    ("duration", "timestamp"),
                    ("timestamp", "duration"),
                ],
            )),
        },
    }
}

fn sub(l: &Val, r: &Val) -> R {
    match (l, r) {
        (Val::Int(a), Val::Int(b)) => a.checked_sub(*b).map(Val::Int).ok_or_else(Error::overflow),
        (Val::Duration(a), Val::Duration(b)) => Val::duration(a - b),
        (Val::Timestamp(s, n), Val::Duration(d)) => {
            Val::timestamp_nanos(timestamp_nanos(*s, *n) - d)
        }
        (Val::Timestamp(s, n), Val::Timestamp(t, m)) => {
            Val::duration(timestamp_nanos(*s, *n) - timestamp_nanos(*t, *m))
        }
        _ => match (number(l), number(r)) {
            (Some(a), Some(b)) => Ok(Val::Float(a - b)),
            _ => Err(unsupported(
                BinaryOp::Sub,
                l,
                r,
                &[
                    ("int", "int"),
                    ("float", "float"),
                    ("duration", "duration"),
                    ("timestamp", "duration"),
                    ("timestamp", "timestamp"),
                ],
            )),
        },
    }
}

/// `*`, `/` and `%`: ints (dividing the smallest int by -1 wraps, as in Java) or floats.
fn multiplicative(op: BinaryOp, l: &Val, r: &Val) -> R {
    if let (Val::Int(a), Val::Int(b)) = (l, r) {
        return match op {
            BinaryOp::Mul => a.checked_mul(*b).map(Val::Int).ok_or_else(Error::overflow),
            _ if *b == 0 => Err(Error::divide_by_zero()),
            BinaryOp::Div => Ok(Val::Int(a.wrapping_div(*b))),
            _ => Ok(Val::Int(a.wrapping_rem(*b))),
        };
    }
    match (number(l), number(r)) {
        (Some(a), Some(b)) => Ok(Val::Float(match op {
            BinaryOp::Mul => a * b,
            BinaryOp::Div => a / b,
            _ => a % b,
        })),
        _ => Err(unsupported(op, l, r, &[("int", "int"), ("float", "float")])),
    }
}

fn number(v: &Val) -> Option<f64> {
    match v {
        Val::Int(i) => Some(*i as f64),
        Val::Float(f) => Some(*f),
        _ => None,
    }
}

pub(crate) fn timestamp_nanos(seconds: i64, nanos: u32) -> i128 {
    i128::from(seconds) * super::value::NANOS + i128::from(nanos)
}

/// An RE2 pattern in the `regex` crate's syntax: `\d`, `\w`, `\s` and `\b` are ASCII-only in
/// RE2, and a bracket, `&` or `~` inside a class is a literal there.
fn re2_to_rust(pattern: &str) -> String {
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
                let ascii = match (class, e) {
                    (false, 'd') => "[0-9]",
                    (false, 'D') => "[^0-9]",
                    (false, 'w') => "[0-9A-Za-z_]",
                    (false, 'W') => "[^0-9A-Za-z_]",
                    (false, 's') => "[\\t\\n\\f\\r ]",
                    (false, 'S') => "[^\\t\\n\\f\\r ]",
                    (false, 'b') => "(?-u:\\b)",
                    (false, 'B') => "(?-u:\\B)",
                    (true, 'd') => "0-9",
                    (true, 'w') => "0-9A-Za-z_",
                    (true, 's') => "\\t\\n\\f\\r ",
                    _ => {
                        out.push('\\');
                        out.push(e);
                        continue;
                    }
                };
                out.push_str(ascii);
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
            ']' if class && at_first => out.push_str("\\]"),
            ']' if class => {
                out.push(']');
                class = false;
            }
            '[' if class && chars.peek() == Some(&':') => {
                // A POSIX class, `[:alpha:]`, as both read it.
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

/// A path literal's segment: `%XX` escapes and `+` for a space, as Java's `URLDecoder` reads
/// them.
pub(crate) fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| char::from(b).to_digit(16);
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(high), Some(low)) => {
                    out.push((high * 16 + low) as u8);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The size of a string in rules: UTF-16 code units.
pub(crate) fn string_size(s: &str) -> i64 {
    utf16_len(s) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re2_patterns() {
        for (re2, rust) in [
            (r"\d+", "[0-9]+"),
            (r"[\d_]", "[0-9_]"),
            (r"\w\s\b", r"[0-9A-Za-z_][\t\n\f\r ](?-u:\b)"),
            ("[]a]", r"[\]a]"),
            ("[^]a]", r"[^\]a]"),
            ("[a&&b]", r"[a\&\&b]"),
            ("[[:alpha:]x]", "[[:alpha:]x]"),
            ("[a[]", r"[a\[]"),
            (r"a\.b", r"a\.b"),
        ] {
            assert_eq!(re2_to_rust(re2), rust, "{re2}");
        }
        let re = |p: &str| regex::Regex::new(&re2_to_rust(p)).unwrap();
        assert!(!re(r"^\w$").is_match("é"));
        assert!(!re(r"^\d$").is_match("٣"));
        assert!(re(r"^.$").is_match("é"));
    }

    #[test]
    fn path_segments_are_url_decoded() {
        assert_eq!(url_decode("a%20b"), "a b");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("%2F"), "/");
        assert_eq!(url_decode("%C3%A9"), "é");
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%zz"), "%zz");
        assert_eq!(url_decode("(default)"), "(default)");
    }
}
