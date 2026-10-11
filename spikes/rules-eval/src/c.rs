//! Evaluator C: A's parser and static checks, translated into cel-rust's AST and evaluated by
//! cel-rust's interpreter with B's Rules library (ADR 0005 option C).

use crate::a::parse::{self, Expr, Kind};
use crate::{Failure, Input};
use cel::common::ast::{
    self, CallExpr, EntryExpr, IdedEntryExpr, IdedExpr, ListExpr, LiteralValue, MapEntryExpr,
    MapExpr, SelectExpr, operators,
};
use cel::common::types::{CelBool, CelBytes, CelDouble, CelInt, CelString};
use cel::{Env, Value};
use serde_json::Value as Json;
use std::sync::Arc;

pub fn run(src: &str, vars: &[(String, Input)]) -> Result<Json, Failure> {
    let expr = parse::parse(src).map_err(Failure::Compile)?;
    crate::a::check(&expr, vars).map_err(Failure::Compile)?;
    let mut ids = 0;
    let ast = translate(&expr, &mut ids);
    let ctx = crate::b::context(Arc::new(Env::default()), vars);
    Value::resolve(&ast, &ctx)
        .map(|v| crate::b::to_json(&v))
        .map_err(|e| Failure::Eval(e.to_string()))
}

fn call(name: &str, target: Option<IdedExpr>, args: Vec<IdedExpr>) -> ast::Expr {
    ast::Expr::Call(CallExpr {
        func_name: name.to_string(),
        target: target.map(Box::new),
        args,
    })
}

fn lit(v: LiteralValue) -> ast::Expr {
    ast::Expr::Literal(v)
}

fn translate(e: &Expr, ids: &mut u64) -> IdedExpr {
    *ids += 1;
    let id = *ids;
    let mut t = |e: &Expr| translate(e, ids);
    let expr = match &e.kind {
        Kind::Null => lit(LiteralValue::Null),
        Kind::Bool(b) => lit(LiteralValue::Boolean(CelBool::from(*b))),
        Kind::Int(i) => lit(LiteralValue::Int(CelInt::from(*i))),
        Kind::Float(f) => lit(LiteralValue::Double(CelDouble::from(*f))),
        Kind::Str(s) => lit(LiteralValue::String(CelString::from(s.clone()))),
        Kind::Bytes(b) => lit(LiteralValue::Bytes(CelBytes::from(b.clone()))),
        Kind::Ident(n) => ast::Expr::Ident(n.clone()),
        Kind::List(items) => ast::Expr::List(ListExpr {
            elements: items.iter().map(&mut t).collect(),
            optional_indices: vec![],
        }),
        Kind::Map(entries) => ast::Expr::Map(MapExpr {
            entries: entries
                .iter()
                .map(|(k, v)| IdedEntryExpr {
                    id: 0,
                    expr: EntryExpr::MapEntry(MapEntryExpr {
                        key: t(k),
                        value: t(v),
                        optional: false,
                    }),
                })
                .collect(),
        }),
        Kind::Path(segs) => {
            let args = segs
                .iter()
                .map(|s| match s {
                    Ok(s) => IdedExpr {
                        id: 0,
                        expr: lit(LiteralValue::String(CelString::from(s.clone()))),
                    },
                    Err(x) => t(x),
                })
                .collect();
            call("@path", None, args)
        }
        Kind::Select(x, f) => ast::Expr::Select(SelectExpr {
            operand: Box::new(t(x)),
            field: f.clone(),
            test: false,
        }),
        Kind::Index(x, i) => {
            let args = vec![t(x), t(i)];
            call(operators::INDEX, None, args)
        }
        Kind::Slice(x, i, j) => {
            let args = vec![t(x), t(i), t(j)];
            call("@slice", None, args)
        }
        Kind::Call(target, f, args) => {
            let target = target.as_ref().map(|x| t(x));
            let args = args.iter().map(&mut t).collect();
            call(f, target, args)
        }
        Kind::Unary(op, x) => {
            let args = vec![t(x)];
            call(
                if *op == "!" {
                    operators::LOGICAL_NOT
                } else {
                    operators::NEGATE
                },
                None,
                args,
            )
        }
        Kind::Binary(op, a, b) => {
            let name = match *op {
                "&&" => operators::LOGICAL_AND,
                "||" => operators::LOGICAL_OR,
                "==" => operators::EQUALS,
                "!=" => operators::NOT_EQUALS,
                "<" => operators::LESS,
                "<=" => operators::LESS_EQUALS,
                ">" => operators::GREATER,
                ">=" => operators::GREATER_EQUALS,
                "+" => operators::ADD,
                "-" => operators::SUBSTRACT,
                "*" => operators::MULTIPLY,
                "/" => operators::DIVIDE,
                "%" => operators::MODULO,
                _ => operators::IN,
            };
            let args = vec![t(a), t(b)];
            call(name, None, args)
        }
        Kind::Is(x, ty) => {
            let args = vec![
                t(x),
                IdedExpr {
                    id: 0,
                    expr: lit(LiteralValue::String(CelString::from(ty.clone()))),
                },
            ];
            call("@is", None, args)
        }
        Kind::Cond(c, a, b) => {
            let args = vec![t(c), t(a), t(b)];
            call(operators::CONDITIONAL, None, args)
        }
    };
    IdedExpr { id, expr }
}
