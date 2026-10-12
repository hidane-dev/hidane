//! The compiled ruleset: what the evaluator walks.
//!
//! Every node keeps its [`Span`]. A ruleset only exists when its file compiled, so nothing in
//! it is an error placeholder.

use std::collections::HashMap;

/// Where a node is: byte offsets into its file (end exclusive) and the line and column
/// (1-based, in code points) of its start, as the official emulator prints them
/// (`evaluation error at L5:22`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub column: u32,
}

/// `rules_version`: `'1'` (or none) and `'2'` differ in how recursive wildcards match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ruleset {
    pub version: Version,
    /// The name of the file it was compiled from, as the request gave it.
    pub file_name: Option<String>,
    /// Functions declared at the top level, outside any service.
    pub functions: Vec<Function>,
    pub services: Vec<Service>,
    /// The opening parentheses around each parenthesized expression, outermost first, by the
    /// expression's byte range. The AST leaves parentheses out; the coverage report and the
    /// positions of evaluation errors count an expression from its parentheses.
    pub parens: HashMap<(usize, usize), Vec<Span>>,
}

impl Ruleset {
    /// The service named `name` (`cloud.firestore`), if declared.
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    /// The dotted name, `cloud.firestore`.
    pub name: String,
    pub name_span: Span,
    pub body: Body,
    pub span: Span,
}

/// What a service or a `match` contains, each kind in source order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Body {
    pub functions: Vec<Function>,
    pub matches: Vec<Match>,
    pub allows: Vec<Allow>,
}

/// `match /path/{var}/{rest=**} { … }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub path: Vec<Segment>,
    pub body: Body,
    /// The `match` keyword.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub kind: SegmentKind,
    /// From the slash to the end of the segment.
    pub span: Span,
    /// The variable's name, or the whole segment for a literal.
    pub name_span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentKind {
    /// `/users`.
    Literal(String),
    /// `/{user}` or `/{user=*}`: one segment.
    Wildcard(String),
    /// `/{rest=**}`: any number of segments.
    Recursive(String),
}

impl SegmentKind {
    /// The variable the segment binds, if any.
    pub fn variable(&self) -> Option<&str> {
        match self {
            Self::Literal(_) => None,
            Self::Wildcard(name) | Self::Recursive(name) => Some(name),
        }
    }
}

/// `allow read, write: if condition;`.
#[derive(Debug, Clone, PartialEq)]
pub struct Allow {
    pub methods: Vec<MethodName>,
    /// `None` for `allow read;`, which always allows.
    pub condition: Option<Expr>,
    /// The `allow` keyword.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodName {
    pub name: String,
    /// `None` when the name is not a method (the compiler warns about it).
    pub method: Option<Method>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Read,
    Write,
    Get,
    List,
    Create,
    Update,
    Delete,
}

impl Method {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "read" => Self::Read,
            "write" => Self::Write,
            "get" => Self::Get,
            "list" => Self::List,
            "create" => Self::Create,
            "update" => Self::Update,
            "delete" => Self::Delete,
            _ => return None,
        })
    }

    /// Whether `self` covers the request method `request` (`read` covers `get` and `list`).
    pub fn covers(self, request: Self) -> bool {
        self == request
            || matches!(
                (self, request),
                (Self::Read, Self::Get | Self::List)
                    | (Self::Write, Self::Create | Self::Update | Self::Delete)
            )
    }
}

/// `function name(params) { let x = …; return expr; }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub lets: Vec<Let>,
    pub result: Expr,
    /// The `function` keyword.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Let {
    pub name: Ident,
    pub value: Expr,
    /// The `let` keyword.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    /// From the first token of the expression to the end of its last.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Null,
    Bool(bool),
    Int(i64),
    /// An integer literal beyond `i64`: it compiles, and fails when evaluated.
    IntOverflow(String),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    /// A variable: `request`, a wildcard, a parameter or a `let`.
    Ident(String),
    List(Vec<Expr>),
    /// `{k: v, …}`, in source order.
    Map(Vec<(Expr, Expr)>),
    /// A path literal: `/databases/$(database)/documents`.
    Path(Vec<PathPart>),
    /// `object.field`.
    Member {
        object: Box<Expr>,
        field: Ident,
    },
    /// `object[index]`.
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    /// `object[start:end]`.
    Slice {
        object: Box<Expr>,
        start: Box<Expr>,
        end: Box<Expr>,
    },
    /// `f(args)`, `math.abs(args)` or `receiver.f(args)`.
    Call {
        callee: Callee,
        args: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        /// The operator token.
        op_span: Span,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `value is type`.
    Is {
        value: Box<Expr>,
        type_name: Ident,
    },
    /// `condition ? then : otherwise`.
    Ternary {
        condition: Box<Expr>,
        /// The `?` token.
        question: Span,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    /// `name(…)`: a function of the ruleset or a global (`get`, `int`, …).
    Global(Ident),
    /// `namespace.name(…)`: `math.abs`, `duration.value`, …
    Namespaced { namespace: Ident, name: Ident },
    /// `receiver.name(…)`.
    Method { receiver: Box<Expr>, name: Ident },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathPart {
    /// A literal segment without its slash.
    Segment(String),
    /// `$(expr)`.
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}
