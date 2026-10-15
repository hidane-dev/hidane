//! The functions of pipeline expressions, by name, as the official emulator knows them
//! (`tests/fixtures/pipelines.json`): each with the signatures its arity errors print, the
//! checks of its literal arguments, made when the pipeline is read, and its evaluation.
//!
//! Most functions read their arguments first: a null or absent one makes the result null, and
//! one of another type than the function takes is an error that names both
//! (`The function add(...) requires `Number` but got `STRING`.`).

use std::{collections::HashMap, sync::LazyLock};

use hidane_proto::google::firestore::v1::Value;
use tonic::Status;

use super::{
    expr::{Call, Eval, Expr, Scope},
    value,
};

mod arrays;
pub(crate) mod compare;
mod construct;
mod control;
mod documents;
mod logic;
mod maps;
mod math;
mod strings;
mod time;
mod types;
mod vectors;

/// A function: how it is called, checked and evaluated.
pub(crate) struct Def {
    pub(crate) name: &'static str,
    pub(crate) signatures: &'static [Signature],
    /// Checks of literal arguments, when the pipeline is read.
    pub(crate) check: Option<fn(&[Expr]) -> Result<(), Status>>,
    pub(crate) eval: fn(&Call, &Scope<'_>) -> Eval,
}

impl std::fmt::Debug for Def {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// One way to call a function, as its errors print it, and how many arguments it takes.
pub(crate) struct Signature {
    pub(crate) text: &'static str,
    pub(crate) min: usize,
    pub(crate) max: Option<usize>,
}

impl Signature {
    pub(crate) const fn fixed(text: &'static str, n: usize) -> Self {
        Self {
            text,
            min: n,
            max: Some(n),
        }
    }

    pub(crate) const fn variadic(text: &'static str, min: usize) -> Self {
        Self {
            text,
            min,
            max: None,
        }
    }

    fn accepts(&self, n: usize) -> bool {
        n >= self.min && self.max.is_none_or(|max| n <= max)
    }

    fn range(&self) -> String {
        match self.max {
            Some(max) => format!("[{}..{max}]", self.min),
            None => format!("[{}..+∞)", self.min),
        }
    }
}

static SCALAR: LazyLock<HashMap<&'static str, &'static Def>> = LazyLock::new(|| {
    [
        arrays::DEFS,
        compare::DEFS,
        construct::DEFS,
        control::DEFS,
        documents::DEFS,
        logic::DEFS,
        maps::DEFS,
        math::DEFS,
        strings::DEFS,
        time::DEFS,
        types::DEFS,
        vectors::DEFS,
    ]
    .into_iter()
    .flatten()
    .map(|def| (def.name, def))
    .collect()
});

/// The scalar function `name`, or the official error for one it does not know.
pub(crate) fn scalar(name: &str) -> Result<&'static Def, Status> {
    SCALAR
        .get(name)
        .copied()
        .ok_or_else(|| does_not_exist(name))
}

pub(crate) fn does_not_exist(name: &str) -> Status {
    Status::invalid_argument(format!("The function '{name}' does not exist."))
}

/// Checks that the function `name`, with `signatures`, takes `n` arguments.
pub(crate) fn check_arity(name: &str, signatures: &[Signature], n: usize) -> Result<(), Status> {
    if signatures.iter().any(|s| s.accepts(n)) {
        return Ok(());
    }
    Err(Status::invalid_argument(match signatures {
        [one] => format!(
            "The function '{}' takes {} argument(s), but {n} {} provided.",
            one.text,
            one.range(),
            if n == 1 { "was" } else { "were" }
        ),
        all => {
            let options: Vec<String> = all
                .iter()
                .map(|s| format!("- {} with {} argument(s).", s.text, s.range()))
                .collect();
            format!(
                "The function '{name}' does not accept {n} argument(s). Valid options are:\n{}",
                options.join("\n")
            )
        }
    }))
}

/// `The function {name}(...) requires `{expected}` but got `{kind}`.`
pub(crate) fn requires(name: &str, expected: &str, got: &Value) -> Status {
    Status::invalid_argument(format!(
        "The function {name}(...) requires `{expected}` but got `{}`.",
        value::kind(got)
    ))
}

/// The values of every argument, in order.
pub(crate) fn args(call: &Call, scope: &Scope<'_>) -> Result<Vec<Option<Value>>, Status> {
    call.args.iter().map(|arg| scope.eval(arg)).collect()
}

/// `Some(null)`: what most functions give for a null or absent argument.
pub(crate) fn null() -> Eval {
    Ok(Some(value::null()))
}

/// The argument of a literal check, as the errors describe what was found instead.
pub(crate) fn found(expr: &Expr) -> &'static str {
    match expr {
        Expr::Field(_) => "a property reference",
        Expr::Variable(_) => "a variable reference",
        Expr::Call(_) => "a function",
        Expr::Pipeline(_) => "a pipeline",
        Expr::Constant(_) | Expr::Array(_) | Expr::Map(_) => "a constant",
    }
}

/// `The argument '{argument}' for function '{function}' must be a literal of type string from
/// the following list: [...]`, unless `expr` is one of `options`.
pub(crate) fn one_of(
    function: &str,
    argument: &str,
    expr: &Expr,
    options: &[&str],
) -> Result<(), Status> {
    if expr.literal_string().is_some_and(|s| options.contains(&s)) {
        return Ok(());
    }
    Err(Status::invalid_argument(format!(
        "The argument '{argument}' for function '{function}' must be a literal of type string \
         from the following list: [{}]",
        options.join(", ")
    )))
}
