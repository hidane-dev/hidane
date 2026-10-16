//! GQL (#81): queries in the language of the GQL reference
//! (https://cloud.google.com/datastore/docs/reference/gql_reference), read into the structured
//! queries they stand for, as the official emulator in Datastore mode reads them
//! (`tests/fixtures/datastore_gql.json`, from `tools/oracle/datastore_gql.py`).
//!
//! The official emulator reports a query it cannot read as a JavaCC-generated parser does: a
//! lexical error names the character and what came before it in the token; a syntax error names
//! the token it met and the tokens it expected there. hidane's parser is written by hand; where
//! it fails, it names the token met and the tokens the official emulator was recorded to expect
//! at that point of the grammar.

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hidane_proto::google::datastore::v1::{
    AggregationQuery, ArrayValue, CompositeFilter, Filter, GqlQuery, GqlQueryParameter, Key,
    KindExpression, PartitionId, Projection, PropertyFilter, PropertyOrder, PropertyReference,
    Query, Value, aggregation_query, composite_filter, filter::FilterType,
    gql_query_parameter::ParameterType, key::PathElement, key::path_element::IdType,
    property_filter, property_order, value::ValueType,
};
use prost_types::Timestamp;
use tonic::Status;

fn invalid(message: impl Into<String>) -> Status {
    Status::invalid_argument(message)
}

// --- tokens ------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Eof,
    Keyword(&'static str),
    Name,
    QuotedName,
    Integer,
    Double,
    SingleQuoted,
    DoubleQuoted,
    NameBinding,
    PositionBinding,
    Punct(&'static str),
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    /// The text as written.
    image: String,
    /// The value: a name or string unquoted and unescaped, a binding without `@`.
    text: String,
    line: usize,
    column: usize,
}

/// The words GQL reads as keywords (the others it reserves are names to the parser).
const KEYWORDS: &[&str] = &[
    "and",
    "asc",
    "by",
    "contains",
    "desc",
    "distinct",
    "false",
    "from",
    "group",
    "has",
    "in",
    "is",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "select",
    "true",
    "where",
    "ancestor",
    "descendant",
];

/// How a JavaCC parser names a token kind in its messages.
fn image_of(kind: &Kind) -> String {
    match kind {
        Kind::Eof => "<EOF>".into(),
        Kind::Keyword(k) | Kind::Punct(k) => format!("\"{k}\""),
        Kind::Name => "<UNQUOTED_NAME>".into(),
        Kind::QuotedName => "<QUOTED_NAME>".into(),
        Kind::Integer => "<INTEGER>".into(),
        Kind::Double => "<DOUBLE>".into(),
        Kind::SingleQuoted => "<SINGLE_QUOTE_STRING>".into(),
        Kind::DoubleQuoted => "<DOUBLE_QUOTE_STRING>".into(),
        Kind::NameBinding => "<NAME_BINDING_SITE>".into(),
        Kind::PositionBinding => "<POSITION_BINDING_SITE>".into(),
    }
}

/// JavaCC's `add_escapes`.
fn add_escapes(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    column: usize,
    text: &'a str,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.column = 0;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    /// A lexical error: `found` (`None` for the end) after `prefix` of the current token.
    fn error(&self, found: Option<char>, prefix: &str) -> Status {
        let at = match found {
            Some(c) => format!("'{}' ({}), ", add_escapes(&c.to_string()), c as u32),
            None => "<EOF> ".into(),
        };
        // The column of the character met, or just past the last one at the end.
        let column = self.column + 1;
        invalid(format!(
            "Lexical error at line {}, column {column}.  Encountered: {at}after prefix \"{}\"",
            self.line.max(1),
            add_escapes(prefix)
        ))
    }

    fn tokens(mut self) -> Result<Vec<Token>, Status> {
        let _ = self.text;
        let mut out = Vec::new();
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            let (line, column) = (self.line, self.column + 1);
            let Some(c) = self.peek() else {
                // JavaCC puts the end where the last character was, or at 0:0 for nothing.
                let (line, column) = if self.chars.is_empty() {
                    (0, 0)
                } else {
                    (self.line, self.column)
                };
                out.push(Token {
                    kind: Kind::Eof,
                    image: String::new(),
                    text: String::new(),
                    line,
                    column,
                });
                return Ok(out);
            };
            let start = self.pos;
            let token = |kind: Kind, lexer: &Self, text: String| Token {
                image: lexer.chars[start..lexer.pos].iter().collect(),
                kind,
                text,
                line,
                column,
            };
            match c {
                '\'' | '"' => {
                    self.bump();
                    let mut text = String::new();
                    loop {
                        let prefix: String = self.chars[start..self.pos].iter().collect();
                        match self.bump() {
                            None => return Err(self.error(None, &prefix)),
                            Some(q) if q == c => {
                                if self.peek() == Some(c) {
                                    self.bump();
                                    text.push(c);
                                } else {
                                    break;
                                }
                            }
                            Some('\\') => {
                                let prefix: String = self.chars[start..self.pos].iter().collect();
                                match self.peek() {
                                    Some(e) => {
                                        let escaped = match e {
                                            '\\' => "\\",
                                            '0' => "\0",
                                            'b' => "\u{8}",
                                            'n' => "\n",
                                            'r' => "\r",
                                            't' => "\t",
                                            'Z' => "\u{1a}",
                                            '\'' => "'",
                                            '"' => "\"",
                                            '`' => "`",
                                            '%' => "\\%",
                                            '_' => "\\_",
                                            other => return Err(self.error(Some(other), &prefix)),
                                        };
                                        self.bump();
                                        text.push_str(escaped);
                                    }
                                    None => return Err(self.error(None, &prefix)),
                                }
                            }
                            Some('\n') => return Err(self.error(Some('\n'), &prefix)),
                            Some(other) => text.push(other),
                        }
                    }
                    let kind = if c == '\'' {
                        Kind::SingleQuoted
                    } else {
                        Kind::DoubleQuoted
                    };
                    out.push(token(kind, &self, text));
                }
                '`' => {
                    self.bump();
                    let mut text = String::new();
                    loop {
                        let prefix: String = self.chars[start..self.pos].iter().collect();
                        match self.bump() {
                            None => return Err(self.error(None, &prefix)),
                            Some('`') => {
                                if self.peek() == Some('`') {
                                    self.bump();
                                    text.push('`');
                                } else {
                                    break;
                                }
                            }
                            Some('\n') => return Err(self.error(Some('\n'), &prefix)),
                            Some(other) => text.push(other),
                        }
                    }
                    out.push(token(Kind::QuotedName, &self, text));
                }
                '@' => {
                    self.bump();
                    let mut text = String::new();
                    while let Some(n) = self.peek().filter(|c| name_char(*c)) {
                        self.bump();
                        text.push(n);
                    }
                    let kind = if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
                        Kind::PositionBinding
                    } else {
                        Kind::NameBinding
                    };
                    out.push(token(kind, &self, text));
                }
                c if c.is_ascii_digit()
                    || ((c == '+' || c == '-' || c == '.')
                        && self
                            .chars
                            .get(self.pos + 1)
                            .is_some_and(|n| n.is_ascii_digit() || (*n == '.' && c != '.'))) =>
                {
                    self.bump();
                    let mut double = c == '.';
                    while let Some(d) = self.peek() {
                        if d.is_ascii_digit() {
                            self.bump();
                        } else if d == '.' && !double {
                            double = true;
                            self.bump();
                        } else if (d == 'e' || d == 'E')
                            && self
                                .chars
                                .get(self.pos + 1)
                                .is_some_and(|n| n.is_ascii_digit() || *n == '+' || *n == '-')
                        {
                            double = true;
                            self.bump();
                            if self.peek().is_some_and(|s| s == '+' || s == '-') {
                                self.bump();
                            }
                        } else {
                            break;
                        }
                    }
                    let text: String = self.chars[start..self.pos].iter().collect();
                    out.push(token(
                        if double { Kind::Double } else { Kind::Integer },
                        &self,
                        text,
                    ));
                }
                c if name_char(c) && !c.is_ascii_digit() => {
                    while self.peek().is_some_and(name_char) {
                        self.bump();
                    }
                    let text: String = self.chars[start..self.pos].iter().collect();
                    let lower = text.to_ascii_lowercase();
                    let kind = match KEYWORDS.iter().find(|k| **k == lower) {
                        Some(k) => Kind::Keyword(k),
                        None => Kind::Name,
                    };
                    out.push(token(kind, &self, text));
                }
                _ => {
                    let two: String = self.chars[self.pos..(self.pos + 2).min(self.chars.len())]
                        .iter()
                        .collect();
                    let punct = [
                        "!=", "<=", ">=", "<", ">", "=", "(", ")", ",", "*", "+", ".",
                    ]
                    .into_iter()
                    .find(|p| two.starts_with(p));
                    match punct {
                        Some(p) => {
                            for _ in 0..p.chars().count() {
                                self.bump();
                            }
                            out.push(token(Kind::Punct(p), &self, p.to_owned()));
                        }
                        None => return Err(self.error(Some(c), "")),
                    }
                }
            }
        }
    }
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || (c as u32) >= 0x80
}

// --- what the parser reads ----------------------------------------------------------------------

/// A value as written: a literal, a binding, or a function of literals.
#[derive(Debug, Clone)]
enum Written {
    Literal(Value, String),
    Named(String),
    Positional(usize),
}

#[derive(Debug, Clone)]
enum Cond {
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Leaf {
        property: String,
        op: property_filter::Operator,
        value: Box<Written>,
    },
}

/// A result position: an integer, or a binding (a cursor or an integer).
#[derive(Debug, Clone)]
enum Position {
    Integer(i64, String),
    Named(String),
    Positional(usize),
}

#[derive(Debug, Default)]
struct Select {
    star: bool,
    properties: Vec<String>,
    distinct: bool,
    distinct_on: Vec<String>,
    kind: Option<String>,
    cond: Option<Cond>,
    orders: Vec<(String, bool)>,
    limit: Option<(Option<Position>, Position)>,
    offset: Option<(Position, Option<Position>)>,
}

/// An aggregation as written: its function (`count`, `count_up_to`, `sum`, `avg`), its
/// argument, and its alias.
type WrittenAggregation = (String, Option<String>, Option<String>);

#[derive(Debug)]
struct Aggregate {
    aggregations: Vec<WrittenAggregation>,
    over: Select,
}

#[derive(Debug)]
enum Parsed {
    Entities(Select),
    Aggregation(Aggregate),
}

const VALUE_START: &[&str] = &[
    "\"false\"",
    "\"null\"",
    "\"true\"",
    "<INTEGER>",
    "<DOUBLE>",
    "<SINGLE_QUOTE_STRING>",
    "<DOUBLE_QUOTE_STRING>",
    "<UNQUOTED_NAME>",
    "<NAME_BINDING_SITE>",
    "<POSITION_BINDING_SITE>",
];

const POSITION_START: &[&str] = &[
    "<INTEGER>",
    "<UNQUOTED_NAME>",
    "<NAME_BINDING_SITE>",
    "<POSITION_BINDING_SITE>",
];

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.at]
    }

    fn next(&mut self) -> Token {
        let token = self.tokens[self.at].clone();
        if self.at + 1 < self.tokens.len() {
            self.at += 1;
        }
        token
    }

    /// A syntax error at the next token, which is none of `expected`.
    fn error(&self, expected: &[&str]) -> Status {
        let token = self.peek();
        let encountered = match token.kind {
            Kind::Eof => "<EOF>".to_owned(),
            ref kind => format!(" {} \"{} \"", image_of(kind), add_escapes(&token.image)),
        };
        let mut message = format!(
            "Encountered \"{encountered}\" at line {}, column {}.\n",
            token.line, token.column
        );
        message.push_str(if expected.len() == 1 {
            "Was expecting:\n    "
        } else {
            "Was expecting one of:\n    "
        });
        for e in expected {
            message.push_str(e);
            message.push(' ');
            if *e != "<EOF>" {
                message.push_str("...");
            }
            message.push_str("\n    ");
        }
        invalid(message)
    }

    fn is_keyword(&self, keyword: &str) -> bool {
        self.peek().kind == Kind::Keyword(keyword_static(keyword))
    }

    fn is_punct(&self, punct: &str) -> bool {
        matches!(self.peek().kind, Kind::Punct(p) if p == punct)
    }

    fn is_name(&self, name: &str) -> bool {
        self.peek().kind == Kind::Name && self.peek().text.eq_ignore_ascii_case(name)
    }

    fn keyword(&mut self, keyword: &str, expected: &[&str]) -> Result<(), Status> {
        if self.is_keyword(keyword) {
            self.next();
            Ok(())
        } else {
            Err(self.error(expected))
        }
    }

    fn punct(&mut self, punct: &str) -> Result<(), Status> {
        if self.is_punct(punct) {
            self.next();
            Ok(())
        } else {
            Err(self.error(&[&format!("\"{punct}\"")]))
        }
    }

    /// A name: unquoted or quoted.
    fn name(&mut self, expected: &[&str]) -> Result<String, Status> {
        match self.peek().kind {
            Kind::Name | Kind::QuotedName => Ok(self.next().text),
            _ => Err(self.error(expected)),
        }
    }

    /// A property name: names joined with `.`.
    fn property(&mut self, expected: &[&str]) -> Result<String, Status> {
        let mut name = self.name(expected)?;
        while self.is_punct(".") {
            self.next();
            name.push('.');
            name.push_str(&self.name(expected)?);
        }
        Ok(name)
    }

    fn query(&mut self) -> Result<Parsed, Status> {
        if self.is_name("aggregate") {
            self.next();
            let aggregations = self.aggregations()?;
            if !self.is_name("over") {
                return Err(self.error(&["\"over\""]));
            }
            self.next();
            self.punct("(")?;
            let over = self.select()?;
            self.punct(")")?;
            if self.peek().kind != Kind::Eof {
                return Err(self.error(&["<EOF>"]));
            }
            let Parsed::Entities(over) = over else {
                return Err(self.error(&["\"select\""]));
            };
            return Ok(Parsed::Aggregation(Aggregate { aggregations, over }));
        }
        let parsed = self.select()?;
        if self.peek().kind != Kind::Eof {
            return Err(self.error(&["<EOF>"]));
        }
        Ok(parsed)
    }

    fn aggregation_start(&self) -> bool {
        ["count", "count_up_to", "sum", "avg"]
            .iter()
            .any(|f| self.is_name(f))
            && matches!(
                self.tokens.get(self.at + 1).map(|t| &t.kind),
                Some(Kind::Punct("("))
            )
    }

    fn aggregations(&mut self) -> Result<Vec<WrittenAggregation>, Status> {
        let mut out = Vec::new();
        loop {
            let function = self.name(&["<UNQUOTED_NAME>"])?.to_ascii_lowercase();
            self.punct("(")?;
            let argument = match function.as_str() {
                "count" => {
                    self.punct("*")?;
                    None
                }
                "count_up_to" => match self.peek().kind {
                    Kind::Integer => Some(self.next().text),
                    _ => return Err(self.error(&["<INTEGER>"])),
                },
                "sum" | "avg" => Some(self.property(&["<UNQUOTED_NAME>", "<QUOTED_NAME>"])?),
                other => {
                    return Err(invalid(format!(
                        "Unknown function \"{}\".",
                        other.to_ascii_uppercase()
                    )));
                }
            };
            self.punct(")")?;
            let alias = if self.is_name("as") || self.is_keyword("as") {
                self.next();
                Some(self.name(&["<UNQUOTED_NAME>"])?)
            } else {
                None
            };
            out.push((function, argument, alias));
            if self.is_punct(",") {
                self.next();
            } else {
                return Ok(out);
            }
        }
    }

    fn select(&mut self) -> Result<Parsed, Status> {
        self.keyword("select", &["\"select\""])?;
        if self.aggregation_start() {
            let aggregations = self.aggregations()?;
            let mut over = Select::default();
            if self.is_keyword("from") {
                self.next();
                over.kind = Some(self.name(&["<UNQUOTED_NAME>", "<QUOTED_NAME>"])?);
            }
            if self.is_keyword("where") {
                self.next();
                over.cond = Some(self.compound()?);
            }
            if self.peek().kind != Kind::Eof && !self.is_punct(")") {
                return Err(self.error(&["<EOF>", "\"where\""]));
            }
            return Ok(Parsed::Aggregation(Aggregate { aggregations, over }));
        }
        let mut select = Select::default();
        if self.is_keyword("distinct") {
            self.next();
            if self.is_keyword("on") {
                self.next();
                self.punct("(")?;
                loop {
                    select
                        .distinct_on
                        .push(self.property(&["<UNQUOTED_NAME>"])?);
                    if self.is_punct(",") {
                        self.next();
                    } else {
                        break;
                    }
                }
                self.punct(")")?;
            } else {
                select.distinct = true;
            }
        }
        if self.is_punct("*") {
            self.next();
            select.star = true;
        } else {
            loop {
                select
                    .properties
                    .push(self.property(&["\"*\"", "<UNQUOTED_NAME>"])?);
                if self.is_punct(",") {
                    self.next();
                } else {
                    break;
                }
            }
        }
        if self.is_keyword("from") {
            self.next();
            select.kind = Some(self.name(&["<UNQUOTED_NAME>", "<QUOTED_NAME>"])?);
        }
        if self.is_keyword("where") {
            self.next();
            select.cond = Some(self.compound()?);
        }
        if self.is_keyword("order") {
            self.next();
            self.keyword("by", &["\"by\""])?;
            loop {
                let property = self.property(&["<UNQUOTED_NAME>"])?;
                let descending = if self.is_keyword("desc") {
                    self.next();
                    true
                } else {
                    if self.is_keyword("asc") {
                        self.next();
                    }
                    false
                };
                select.orders.push((property, descending));
                if self.is_punct(",") {
                    self.next();
                } else {
                    break;
                }
            }
        }
        if self.is_keyword("limit") {
            self.next();
            if self.is_name("first") {
                self.next();
                self.punct("(")?;
                let cursor = self.position()?;
                self.punct(",")?;
                let count = self.position()?;
                self.punct(")")?;
                select.limit = Some((Some(cursor), count));
            } else {
                select.limit = Some((None, self.position()?));
            }
            if !self.is_keyword("offset") && self.peek().kind != Kind::Eof && !self.is_punct(")") {
                return Err(self.error(&["<EOF>", "\"offset\""]));
            }
        }
        if self.is_keyword("offset") {
            self.next();
            let first = self.position()?;
            let plus = if self.is_punct("+") {
                self.next();
                Some(self.position()?)
            } else {
                None
            };
            select.offset = Some((first, plus));
        }
        if self.peek().kind != Kind::Eof && !self.is_punct(")") {
            return Err(self.error(&[
                "<EOF>",
                "\"group\"",
                "\"limit\"",
                "\"offset\"",
                "\"order\"",
                "\"where\"",
            ]));
        }
        Ok(Parsed::Entities(select))
    }

    fn position(&mut self) -> Result<Position, Status> {
        match self.peek().kind {
            Kind::Integer => {
                let token = self.next();
                let n = parse_integer(&token.text)?;
                Ok(Position::Integer(n, token.image))
            }
            Kind::NameBinding => Ok(Position::Named(self.next().text)),
            Kind::PositionBinding => {
                Ok(Position::Positional(self.next().text.parse().unwrap_or(0)))
            }
            _ => Err(self.error(POSITION_START)),
        }
    }

    /// Conditions joined by `AND` (binding tighter) and `OR`, and parentheses.
    fn compound(&mut self) -> Result<Cond, Status> {
        let mut ors = vec![self.conjunction()?];
        while self.is_keyword("or") {
            self.next();
            ors.push(self.conjunction()?);
        }
        Ok(if ors.len() == 1 {
            ors.pop().expect("one")
        } else {
            Cond::Or(ors)
        })
    }

    fn conjunction(&mut self) -> Result<Cond, Status> {
        let mut ands = vec![self.term()?];
        while self.is_keyword("and") {
            self.next();
            ands.push(self.term()?);
        }
        Ok(if ands.len() == 1 {
            ands.pop().expect("one")
        } else {
            Cond::And(ands)
        })
    }

    fn term(&mut self) -> Result<Cond, Status> {
        if self.is_punct("(") {
            self.next();
            let inner = self.compound()?;
            self.punct(")")?;
            return Ok(inner);
        }
        // A property first (a name not followed by `(`), or a value first.
        let token = self.peek().clone();
        let property_first = matches!(token.kind, Kind::Name | Kind::QuotedName)
            && !matches!(
                self.tokens.get(self.at + 1).map(|t| &t.kind),
                Some(Kind::Punct("("))
            );
        if property_first {
            let property = self.property(&["\"(\""])?;
            use property_filter::Operator as O;
            if self.is_keyword("is") {
                self.next();
                self.keyword("null", &["\"null\""])?;
                return Ok(Cond::Leaf {
                    property,
                    op: O::Equal,
                    value: Box::new(Written::Literal(
                        Value {
                            value_type: Some(ValueType::NullValue(0)),
                            ..Value::default()
                        },
                        "NULL".into(),
                    )),
                });
            }
            let op = if self.is_keyword("contains") {
                self.next();
                O::Equal
            } else if self.is_keyword("has") {
                self.next();
                self.keyword("ancestor", &["\"ancestor\""])?;
                O::HasAncestor
            } else if self.is_keyword("in") {
                self.next();
                O::In
            } else if self.is_keyword("not") {
                self.next();
                self.keyword("in", &["\"in\""])?;
                O::NotIn
            } else {
                let op = match self.peek().kind {
                    Kind::Punct("=") => O::Equal,
                    Kind::Punct("!=") => O::NotEqual,
                    Kind::Punct("<") => O::LessThan,
                    Kind::Punct("<=") => O::LessThanOrEqual,
                    Kind::Punct(">") => O::GreaterThan,
                    Kind::Punct(">=") => O::GreaterThanOrEqual,
                    _ => {
                        return Err(
                            self.error(&["\"=\"", "\"!=\"", "\"<\"", "\"<=\"", "\">\"", "\">=\""])
                        );
                    }
                };
                self.next();
                op
            };
            let value = self.value()?;
            return Ok(Cond::Leaf {
                property,
                op,
                value: Box::new(value),
            });
        }
        if !matches!(
            token.kind,
            Kind::Integer
                | Kind::Double
                | Kind::SingleQuoted
                | Kind::DoubleQuoted
                | Kind::NameBinding
                | Kind::PositionBinding
                | Kind::Keyword("true" | "false" | "null")
                | Kind::Name
        ) {
            return Err(self.error(&["\"(\""]));
        }
        let value = self.value()?;
        use property_filter::Operator as O;
        let op = if self.is_keyword("in") {
            self.next();
            O::Equal
        } else if self.is_keyword("has") {
            self.next();
            self.keyword("descendant", &["\"descendant\""])?;
            O::HasAncestor
        } else {
            let op = match self.peek().kind {
                Kind::Punct("=") => O::Equal,
                Kind::Punct("!=") => O::NotEqual,
                // A value first: the comparison turned around.
                Kind::Punct("<") => O::GreaterThan,
                Kind::Punct("<=") => O::GreaterThanOrEqual,
                Kind::Punct(">") => O::LessThan,
                Kind::Punct(">=") => O::LessThanOrEqual,
                _ => {
                    return Err(
                        self.error(&["\"=\"", "\"!=\"", "\"<\"", "\"<=\"", "\">\"", "\">=\""])
                    );
                }
            };
            self.next();
            op
        };
        let property = self.property(&["<UNQUOTED_NAME>", "<QUOTED_NAME>"])?;
        Ok(Cond::Leaf {
            property,
            op,
            value: Box::new(value),
        })
    }

    fn value(&mut self) -> Result<Written, Status> {
        let token = self.peek().clone();
        let literal = |value_type: ValueType| {
            Written::Literal(
                Value {
                    value_type: Some(value_type),
                    ..Value::default()
                },
                token.image.clone(),
            )
        };
        let written = match &token.kind {
            Kind::Keyword("true") => literal(ValueType::BooleanValue(true)),
            Kind::Keyword("false") => literal(ValueType::BooleanValue(false)),
            Kind::Keyword("null") => literal(ValueType::NullValue(0)),
            Kind::Integer => literal(ValueType::IntegerValue(parse_integer(&token.text)?)),
            Kind::Double => literal(ValueType::DoubleValue(parse_double(&token.text))),
            Kind::SingleQuoted | Kind::DoubleQuoted => {
                literal(ValueType::StringValue(token.text.clone()))
            }
            Kind::NameBinding => Written::Named(token.text.clone()),
            Kind::PositionBinding => Written::Positional(token.text.parse().unwrap_or(0)),
            Kind::Name => {
                self.next();
                return self.function(&token);
            }
            _ => return Err(self.error(VALUE_START)),
        };
        self.next();
        Ok(written)
    }

    /// `KEY(…)`, `ARRAY(…)`, `BLOB('…')` or `DATETIME('…')`.
    fn function(&mut self, name: &Token) -> Result<Written, Status> {
        let function = name.text.to_ascii_uppercase();
        if !["KEY", "ARRAY", "BLOB", "DATETIME"].contains(&function.as_str()) {
            return Err(invalid(format!("Unknown function \"{}\".", name.text)));
        }
        self.punct("(")?;
        let value_type = match function.as_str() {
            "KEY" => ValueType::KeyValue(self.key()?),
            "ARRAY" => {
                let mut values = Vec::new();
                loop {
                    match self.value()? {
                        Written::Literal(v, _) => values.push(v),
                        _ => return Err(invalid("A binding site may not be in an ARRAY.")),
                    }
                    if self.is_punct(",") {
                        self.next();
                    } else {
                        break;
                    }
                }
                ValueType::ArrayValue(ArrayValue { values })
            }
            "BLOB" => {
                let text = self.string()?;
                let bytes = (!text.contains('='))
                    .then(|| URL_SAFE_NO_PAD.decode(&text).ok())
                    .flatten()
                    .ok_or_else(|| invalid(format!("Invalid blob text: \"{text}\".")))?;
                ValueType::BlobValue(bytes)
            }
            _ => {
                let text = self.string()?;
                ValueType::TimestampValue(parse_datetime(&text).ok_or_else(|| {
                    invalid(format!(
                        "Invalid datetime text (does not match pattern): \"{text}\""
                    ))
                })?)
            }
        };
        self.punct(")")?;
        Ok(Written::Literal(
            Value {
                value_type: Some(value_type),
                ..Value::default()
            },
            function,
        ))
    }

    fn string(&mut self) -> Result<String, Status> {
        match self.peek().kind {
            Kind::SingleQuoted | Kind::DoubleQuoted => Ok(self.next().text),
            _ => Err(self.error(&["<SINGLE_QUOTE_STRING>", "<DOUBLE_QUOTE_STRING>"])),
        }
    }

    fn key(&mut self) -> Result<Key, Status> {
        let mut partition = PartitionId::default();
        if self.is_name("project")
            && matches!(
                self.tokens.get(self.at + 1).map(|t| &t.kind),
                Some(Kind::Punct("("))
            )
        {
            self.next();
            self.punct("(")?;
            partition.project_id = self.string()?;
            self.punct(")")?;
            self.punct(",")?;
        }
        if self.is_name("namespace")
            && matches!(
                self.tokens.get(self.at + 1).map(|t| &t.kind),
                Some(Kind::Punct("("))
            )
        {
            self.next();
            self.punct("(")?;
            partition.namespace_id = self.string()?;
            self.punct(")")?;
            self.punct(",")?;
        }
        let mut path = Vec::new();
        loop {
            let kind = self.name(&["<UNQUOTED_NAME>", "<QUOTED_NAME>"])?;
            self.punct(",")?;
            let id = match self.peek().kind {
                Kind::Integer => IdType::Id(parse_integer(&self.next().text)?),
                Kind::SingleQuoted | Kind::DoubleQuoted => IdType::Name(self.next().text),
                _ => {
                    return Err(self.error(&[
                        "<INTEGER>",
                        "<SINGLE_QUOTE_STRING>",
                        "<DOUBLE_QUOTE_STRING>",
                    ]));
                }
            };
            path.push(PathElement {
                kind,
                id_type: Some(id),
            });
            if self.is_punct(",") {
                self.next();
            } else {
                break;
            }
        }
        Ok(Key {
            partition_id: Some(partition),
            path,
        })
    }
}

fn keyword_static(keyword: &str) -> &'static str {
    KEYWORDS
        .iter()
        .find(|k| **k == keyword)
        .copied()
        .unwrap_or("")
}

fn parse_integer(text: &str) -> Result<i64, Status> {
    text.strip_prefix('+').unwrap_or(text).parse().map_err(|_| {
        invalid(format!(
            "Invalid integer literal: For input string: \"{text}\""
        ))
    })
}

fn parse_double(text: &str) -> f64 {
    let text = text.strip_prefix('+').unwrap_or(text);
    let text = if text.ends_with('.') {
        format!("{text}0")
    } else {
        text.to_owned()
    };
    let text = if let Some(rest) = text.strip_prefix("-.") {
        format!("-0.{rest}")
    } else if let Some(rest) = text.strip_prefix('.') {
        format!("0.{rest}")
    } else {
        text
    };
    text.parse().unwrap_or(f64::NAN)
}

/// `YYYY-MM-DDThh:mm:ss[.S…]` then `Z` or an offset other than ±00:00.
fn parse_datetime(text: &str) -> Option<Timestamp> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let s = text.get(range)?;
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())
            .flatten()
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &text[19..];
    let mut nanos = 0i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let padded = format!("{:0<9}", &fraction[..digits]);
        nanos = padded.parse().ok()?;
        rest = &fraction[digits..];
    }
    let offset = if rest == "Z" {
        0
    } else {
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if rest.len() != 6 || rest.as_bytes()[3] != b':' {
            return None;
        }
        let (h, m): (i64, i64) = (rest[1..3].parse().ok()?, rest[4..6].parse().ok()?);
        if h == 0 && m == 0 {
            return None;
        }
        sign * (h * 3600 + m * 60)
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    #[allow(clippy::cast_possible_truncation)]
    Some(Timestamp {
        seconds,
        nanos: (nanos - nanos % 1_000) as i32,
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// --- bindings and translation -------------------------------------------------------------------

struct Bindings<'a> {
    named: HashMap<&'a str, &'a GqlQueryParameter>,
    positional: &'a [GqlQueryParameter],
    used: Vec<bool>,
    allow_literals: bool,
}

impl Bindings<'_> {
    fn parameter(&mut self, site: &Written) -> Result<Option<&GqlQueryParameter>, Status> {
        Ok(match site {
            Written::Literal(..) => None,
            Written::Named(name) => Some(
                self.named
                    .get(name.as_str())
                    .copied()
                    .ok_or_else(|| invalid(format!("Absent named binding: {name}.")))?,
            ),
            Written::Positional(n) => {
                let parameter = n
                    .checked_sub(1)
                    .and_then(|i| self.positional.get(i))
                    .ok_or_else(|| invalid(format!("Absent positional binding: {n}.")))?;
                self.used[n - 1] = true;
                Some(parameter)
            }
        })
    }

    fn value(&mut self, written: &Written) -> Result<Value, Status> {
        if let Written::Literal(value, image) = written {
            if !self.allow_literals {
                return Err(invalid(format!("Disallowed literal: {image}.")));
            }
            return Ok(value.clone());
        }
        let site = match written {
            Written::Named(name) => format!("@{name}"),
            Written::Positional(n) => format!("@{n}"),
            Written::Literal(..) => unreachable!("handled above"),
        };
        match self
            .parameter(written)?
            .and_then(|p| p.parameter_type.as_ref())
        {
            Some(ParameterType::Value(value)) => Ok(value.clone()),
            _ => Err(invalid(format!(
                "Binding site {site} for value bound to non-value parameter."
            ))),
        }
    }

    /// A position: an integer, or a cursor (`Err` with the cursor).
    fn position(&mut self, position: &Position) -> Result<Result<i64, Vec<u8>>, Status> {
        let site = match position {
            Position::Integer(n, image) => {
                if !self.allow_literals {
                    return Err(invalid(format!("Disallowed literal: {image}.")));
                }
                return Ok(Ok(*n));
            }
            Position::Named(name) => Written::Named(name.clone()),
            Position::Positional(n) => Written::Positional(*n),
        };
        match self
            .parameter(&site)?
            .and_then(|p| p.parameter_type.as_ref())
        {
            Some(ParameterType::Cursor(cursor)) => Ok(Err(cursor.clone())),
            Some(ParameterType::Value(Value {
                value_type: Some(ValueType::IntegerValue(n)),
                ..
            })) => Ok(Ok(*n)),
            _ => Err(invalid("A GQL query parameter is empty.")),
        }
    }
}

fn reference(name: &str) -> PropertyReference {
    PropertyReference {
        name: name.to_owned(),
    }
}

fn translate_cond(cond: &Cond, bindings: &mut Bindings<'_>) -> Result<Filter, Status> {
    let filter_type = match cond {
        Cond::And(children) | Cond::Or(children) => {
            let op = if matches!(cond, Cond::And(_)) {
                composite_filter::Operator::And
            } else {
                composite_filter::Operator::Or
            };
            FilterType::CompositeFilter(CompositeFilter {
                op: op as i32,
                filters: children
                    .iter()
                    .map(|c| translate_cond(c, bindings))
                    .collect::<Result<_, _>>()?,
            })
        }
        Cond::Leaf {
            property,
            op,
            value,
        } => FilterType::PropertyFilter(PropertyFilter {
            property: Some(reference(property)),
            op: *op as i32,
            value: Some(bindings.value(value)?),
        }),
    };
    Ok(Filter {
        filter_type: Some(filter_type),
    })
}

fn translate_select(select: &Select, bindings: &mut Bindings<'_>) -> Result<Query, Status> {
    let mut query = Query::default();
    if let Some(kind) = &select.kind {
        query.kind = vec![KindExpression { name: kind.clone() }];
    }
    if !select.star {
        query.projection = select
            .properties
            .iter()
            .map(|p| Projection {
                property: Some(reference(p)),
            })
            .collect();
    }
    if select.distinct {
        query.distinct_on = select.properties.iter().map(|p| reference(p)).collect();
    } else {
        query.distinct_on = select.distinct_on.iter().map(|p| reference(p)).collect();
    }
    if let Some(cond) = &select.cond {
        query.filter = Some(translate_cond(cond, bindings)?);
    }
    query.order = select
        .orders
        .iter()
        .map(|(property, descending)| PropertyOrder {
            property: Some(reference(property)),
            direction: if *descending {
                property_order::Direction::Descending as i32
            } else {
                property_order::Direction::Ascending as i32
            },
        })
        .collect();
    if let Some((cursor, count)) = &select.limit {
        if let Some(cursor) = cursor {
            match bindings.position(cursor)? {
                Err(bytes) => query.start_cursor = bytes,
                Ok(n) => query.offset = i32::try_from(n).unwrap_or(i32::MAX),
            }
        }
        match bindings.position(count)? {
            Ok(n) => {
                if n < 0 {
                    return Err(invalid(format!("The limit {n} is < 0.")));
                }
                query.limit = Some(i32::try_from(n).unwrap_or(i32::MAX));
            }
            Err(bytes) => query.end_cursor = bytes,
        }
    }
    if let Some((first, plus)) = &select.offset {
        match bindings.position(first)? {
            Ok(n) => query.offset = i32::try_from(n).unwrap_or(i32::MAX),
            Err(bytes) => query.start_cursor = bytes,
        }
        if let Some(plus) = plus
            && let Ok(n) = bindings.position(plus)?
        {
            query.offset = i32::try_from(n).unwrap_or(i32::MAX);
        }
    }
    Ok(query)
}

/// What a GQL query reads as.
pub enum Read {
    Entities(Query),
    Aggregation(AggregationQuery),
}

/// Reads `gql`: its bindings checked, its text parsed and translated.
pub fn read(gql: &GqlQuery) -> Result<Read, Status> {
    for name in gql.named_bindings.keys() {
        if name.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(invalid(format!(
                "GQL query parameter name \"{name}\" starts with a digit."
            )));
        }
    }
    if gql
        .named_bindings
        .values()
        .chain(&gql.positional_bindings)
        .any(|p| p.parameter_type.is_none())
    {
        return Err(invalid("A GQL query parameter is empty."));
    }
    let tokens = Lexer {
        chars: gql.query_string.chars().collect(),
        pos: 0,
        line: 1,
        column: 0,
        text: &gql.query_string,
    }
    .tokens()?;
    let parsed = Parser { tokens, at: 0 }.query()?;
    let mut bindings = Bindings {
        named: gql
            .named_bindings
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect(),
        positional: &gql.positional_bindings,
        used: vec![false; gql.positional_bindings.len()],
        allow_literals: gql.allow_literals,
    };
    let read = match parsed {
        Parsed::Entities(select) => Read::Entities(translate_select(&select, &mut bindings)?),
        Parsed::Aggregation(aggregate) => {
            let nested = translate_select(&aggregate.over, &mut bindings)?;
            let mut aggregations = Vec::new();
            for (function, argument, alias) in aggregate.aggregations {
                use aggregation_query::aggregation::{Avg, Count, Operator, Sum};
                let operator = match function.as_str() {
                    "count" => Operator::Count(Count::default()),
                    "count_up_to" => Operator::Count(Count {
                        up_to: argument.as_deref().map(parse_integer).transpose()?,
                    }),
                    "sum" => Operator::Sum(Sum {
                        property: argument.as_deref().map(reference),
                    }),
                    _ => Operator::Avg(Avg {
                        property: argument.as_deref().map(reference),
                    }),
                };
                aggregations.push(aggregation_query::Aggregation {
                    alias: alias.unwrap_or_default(),
                    operator: Some(operator),
                });
            }
            Read::Aggregation(AggregationQuery {
                aggregations,
                query_type: Some(aggregation_query::QueryType::NestedQuery(nested)),
            })
        }
    };
    if let Some(unused) = bindings.used.iter().position(|used| !used) {
        return Err(invalid(format!(
            "Unused positional binding: {}.",
            unused + 1
        )));
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gql(text: &str) -> Result<Read, Status> {
        read(&GqlQuery {
            query_string: text.into(),
            allow_literals: true,
            ..GqlQuery::default()
        })
    }

    #[test]
    fn syntax_errors_read_as_javacc_writes_them() {
        let message = |text: &str| gql(text).err().unwrap().message().to_owned();
        assert_eq!(
            message(""),
            "Encountered \"<EOF>\" at line 0, column 0.\nWas expecting:\n    \"select\" ...\n    "
        );
        assert_eq!(
            message("SELECT * FROM P extra"),
            "Encountered \" <UNQUOTED_NAME> \"extra \"\" at line 1, column 17.\nWas expecting one of:\n    <EOF> \n    \"group\" ...\n    \"limit\" ...\n    \"offset\" ...\n    \"order\" ...\n    \"where\" ...\n    "
        );
        assert_eq!(
            message("SELECT * FROM T WHERE v = 'a\\qb'"),
            "Lexical error at line 1, column 30.  Encountered: 'q' (113), after prefix \"\\'a\\\\\""
        );
    }

    #[test]
    fn datetimes_read_with_offsets() {
        let ts = parse_datetime("2021-06-01T21:00:00.500000+09:00").unwrap();
        assert_eq!(ts.seconds, 1_622_548_800);
        assert_eq!(ts.nanos, 500_000_000);
        assert!(parse_datetime("2020-01-01").is_none());
        assert!(parse_datetime("2020-01-01T00:00:00+00:00").is_none());
    }
}
