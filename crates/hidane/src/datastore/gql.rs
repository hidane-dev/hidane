//! GQL (#81): queries in the language of the GQL reference
//! (https://cloud.google.com/datastore/docs/reference/gql_reference), read into the structured
//! queries they stand for, as the official emulator in Datastore mode reads them
//! (`tests/fixtures/datastore_gql.json`, from `tools/oracle/datastore_gql.py`).
//!
//! The official emulator reports a query it cannot read as a JavaCC-generated parser does: a
//! lexical error names the character and what came before it in the token; a syntax error names
//! the token it met and the tokens it expected there. hidane's parser is written by hand; where
//! it fails, it names the token met and the tokens the official emulator was recorded to expect
//! at that point of the grammar, and it lexes, checks and refuses in the official emulator's
//! order (`tests/fixtures/datastore_gql_errors.json`, from
//! `tools/oracle/datastore_gql_errors.py`).

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

use super::keys;

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

/// The words GQL reads as keywords, whatever their case (the others it reserves, such as `KEY`
/// or `FIRST`, are names to the parser).
const KEYWORDS: &[&str] = &[
    "aggregate",
    "ancestor",
    "and",
    "as",
    "asc",
    "avg",
    "by",
    "child",
    "contains",
    "count",
    "count_up_to",
    "desc",
    "descendant",
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
    "over",
    "parent",
    "select",
    "sum",
    "true",
    "where",
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

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    column: usize,
}

impl Lexer {
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

    /// A lexical error: `found` (`None` for the end) after `prefix` of the current token, as
    /// JavaCC writes it. JavaCC leaves the prefix out when it read at most one character, the
    /// end not counted (`'` at the end), and the official emulator drops the empty prefix.
    fn error(&self, found: Option<char>, prefix: &str) -> Status {
        let at = match found {
            Some(c) => format!("'{}' ({}), ", add_escapes(&c.to_string()), c as u32),
            None => "<EOF> ".into(),
        };
        // The column of the character met, or just past the last one at the end.
        let column = self.column + 1;
        let message = format!(
            "Lexical error at line {}, column {column}.  Encountered: {at}",
            self.line.max(1)
        );
        if prefix.is_empty() || (found.is_none() && prefix.chars().count() <= 1) {
            invalid(message.trim_end())
        } else {
            invalid(format!("{message}after prefix \"{}\"", add_escapes(prefix)))
        }
    }

    /// The next token, lexed when the parser needs it.
    fn next_token(&mut self) -> Result<Token, Status> {
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
            return Ok(Token {
                kind: Kind::Eof,
                image: String::new(),
                text: String::new(),
                line,
                column,
            });
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
            '\'' | '"' | '`' => {
                self.bump();
                let kind = match c {
                    '\'' => Kind::SingleQuoted,
                    '"' => Kind::DoubleQuoted,
                    _ => Kind::QuotedName,
                };
                let mut text = String::new();
                // Where a quote that a quote follows ended the token: JavaCC goes back
                // there when what the second quote began does not end.
                let mut accepted: Option<(usize, usize, usize, String)> = None;
                let failed = loop {
                    let prefix: String = self.chars[start..self.pos].iter().collect();
                    match self.bump() {
                        None => break self.error(None, &prefix),
                        Some(q) if q == c => {
                            if self.peek() == Some(c) {
                                accepted = Some((self.pos, self.line, self.column, text.clone()));
                                self.bump();
                                text.push(c);
                            } else {
                                return Ok(token(kind, self, text));
                            }
                        }
                        Some('\\') if c != '`' => {
                            let prefix: String = self.chars[start..self.pos].iter().collect();
                            let Some(e) = self.peek() else {
                                break self.error(None, &prefix);
                            };
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
                                other => break self.error(Some(other), &prefix),
                            };
                            self.bump();
                            text.push_str(escaped);
                        }
                        Some('\n') => break self.error(Some('\n'), &prefix),
                        Some(other) => text.push(other),
                    }
                };
                let Some((pos, line, column, text)) = accepted else {
                    return Err(failed);
                };
                (self.pos, self.line, self.column) = (pos, line, column);
                Ok(token(kind, self, text))
            }
            '@' => {
                self.bump();
                // A binding site has a name or a number.
                match self.peek() {
                    None => return Err(self.error(None, "@")),
                    Some(n) if !name_char(n) => return Err(self.error(Some(n), "@")),
                    Some(_) => {}
                }
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
                Ok(token(kind, self, text))
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
                    } else if d == 'e' || d == 'E' {
                        // An exponent's digits may be missing ("1e", a double that does not
                        // read); its sign comes only with them.
                        double = true;
                        self.bump();
                        if self.peek().is_some_and(|s| s == '+' || s == '-')
                            && self
                                .chars
                                .get(self.pos + 1)
                                .is_some_and(char::is_ascii_digit)
                        {
                            self.bump();
                        }
                        while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                            self.bump();
                        }
                        break;
                    } else {
                        break;
                    }
                }
                let text: String = self.chars[start..self.pos].iter().collect();
                Ok(token(
                    if double { Kind::Double } else { Kind::Integer },
                    self,
                    text,
                ))
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
                Ok(token(kind, self, text))
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
                        Ok(token(Kind::Punct(p), self, p.to_owned()))
                    }
                    // `!` begins `!=` only.
                    None if c == '!' => {
                        self.bump();
                        match self.peek() {
                            Some(n) if self.pos + 1 == self.chars.len() => {
                                self.bump();
                                Err(self.error(None, &format!("!{n}")))
                            }
                            Some(n) => Err(self.error(Some(n), "!")),
                            None => Err(self.error(None, "")),
                        }
                    }
                    // A character no token begins with: at the end, the end is met.
                    None if self.pos + 1 == self.chars.len() => {
                        self.bump();
                        Err(self.error(None, ""))
                    }
                    None => Err(self.error(Some(c), "")),
                }
            }
        }
    }
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || (c as u32) >= 0x80
}

// --- the parser ---------------------------------------------------------------------------------
//
// The official emulator's parser is JavaCC's: it lexes a token when it needs it, so a lexical
// error comes only once the parser reaches it, and it acts on each part as it reads it, so a
// binding that is not given or a literal that is not allowed is refused where it is written,
// before the rest of the text is read. It has lexed one token more by then, as it has before it
// reports a syntax error: a lexical error there comes first. A token is the longest that lexes,
// so a quote doubled, then not closed, ends the token at the first. A syntax error lists what
// JavaCC lists at that point: the tokens the grammar allowed there, in the order JavaCC numbers
// them, then, repeated, those its lookaheads tried there. The lists below are those.

/// What a request reads a text as: `RunQuery` entity queries, `RunAggregationQuery`
/// aggregations. Each refuses the other kind as soon as it tells it apart.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Entities,
    Aggregation,
}

const ENTITY_QUERY_EXPECTED: &str = "Expecting ENTITY_QUERY, but got AGGREGATION_QUERY. Use Datastore.RunAggregationQuery API instead for such queries.";
const AGGREGATION_QUERY_EXPECTED: &str = "Expecting AGGREGATION_QUERY, but got ENTITY_QUERY. Use Datastore.RunQuery API instead for such queries.";

const EOF: &str = "<EOF>";
const N: &str = "<UNQUOTED_NAME>";
const QN: &str = "<QUOTED_NAME>";
const INT: &str = "<INTEGER>";
const SQ: &str = "<SINGLE_QUOTE_STRING>";
const DQ: &str = "<DOUBLE_QUOTE_STRING>";
const NB: &str = "<NAME_BINDING_SITE>";
const PB: &str = "<POSITION_BINDING_SITE>";

/// What may start a value.
const VALUE: &[&str] = &[
    "\"false\"",
    "\"null\"",
    "\"true\"",
    INT,
    "<DOUBLE>",
    SQ,
    DQ,
    N,
    NB,
    PB,
];
/// What may start an aggregation: a function.
const FUNCTION: &[&str] = &["\"avg\"", "\"count\"", "\"count_up_to\"", "\"sum\"", N];
/// What may follow a property and start a condition's operator.
const OPERATOR: &[&str] = &[
    "\"contains\"",
    "\"has\"",
    "\"in\"",
    "\"is\"",
    "\"not\"",
    "\"=\"",
    "\"!=\"",
    "\"<\"",
    "\"<=\"",
    "\">\"",
    "\">=\"",
    "\".\"",
];
/// What may follow a value that comes first in a condition.
const REVERSED_OPERATOR: &[&str] = &[
    "\"has\"", "\"in\"", "\"=\"", "\"!=\"", "\"<\"", "\"<=\"", "\">\"", "\">=\"",
];
const POSITION: &[&str] = &[INT, NB, PB];
const NAME: &[&str] = &[N, QN];

/// Where a select's clauses end: at the end of the text, before the `)` of `OVER (…)`, or at
/// the end of a `SELECT` of aggregations.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Within {
    Top,
    Over,
}

/// A condition: a property compared with a value, or conditions joined.
enum Cond {
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Leaf {
        property: String,
        op: property_filter::Operator,
        value: Value,
    },
}

/// What a select reads: the query, and whether its properties were listed (`SELECT a, b`) or
/// not (`SELECT *`).
struct Select {
    query: Query,
}

struct Parser<'a> {
    lexer: Lexer,
    tokens: Vec<Token>,
    /// The lexical error met after `tokens`, once met.
    lexical: Option<Status>,
    at: usize,
    bindings: Bindings<'a>,
    /// A refusal that comes once the whole text is read.
    refused: Option<Status>,
    /// Whether a key value named an invalid project before any refusal: an aggregation's
    /// internal error on the official emulator.
    bad_project_first: bool,
    /// The request's namespace, a key's when it names no partition.
    namespace: &'a str,
}

impl<'a> Parser<'a> {
    /// The token `ahead` tokens on, lexed now if it was not.
    fn look(&mut self, ahead: usize) -> Result<&Token, Status> {
        while self.tokens.len() <= self.at + ahead {
            if let Some(error) = &self.lexical {
                return Err(error.clone());
            }
            if self.tokens.last().is_some_and(|t| t.kind == Kind::Eof) {
                let eof = self.tokens.last().expect("checked").clone();
                self.tokens.push(eof);
                continue;
            }
            match self.lexer.next_token() {
                Ok(token) => self.tokens.push(token),
                Err(error) => {
                    self.lexical = Some(error.clone());
                    return Err(error);
                }
            }
        }
        Ok(&self.tokens[self.at + ahead])
    }

    fn peek(&mut self) -> Result<&Token, Status> {
        self.look(0)
    }

    /// The next token, consumed. JavaCC has lexed the token after it before it acts on it, so
    /// a lexical error there comes first.
    fn next(&mut self) -> Result<Token, Status> {
        let token = self.peek()?.clone();
        self.at += 1;
        if token.kind != Kind::Eof {
            self.peek()?;
        }
        Ok(token)
    }

    fn kind(&mut self) -> Result<Kind, Status> {
        Ok(self.peek()?.kind.clone())
    }

    fn is_keyword(&mut self, keyword: &str) -> Result<bool, Status> {
        Ok(matches!(self.peek()?.kind, Kind::Keyword(k) if k == keyword))
    }

    fn is_punct(&mut self, punct: &str) -> Result<bool, Status> {
        Ok(matches!(self.peek()?.kind, Kind::Punct(p) if p == punct))
    }

    /// A syntax error at the next token, which is none of `expected`. JavaCC has lexed the
    /// token after it too, so a lexical error there comes first.
    fn error(&mut self, expected: &[&str]) -> Status {
        let token = match self.peek() {
            Ok(token) => token.clone(),
            Err(lexical) => return lexical,
        };
        if token.kind != Kind::Eof
            && let Err(lexical) = self.look(1)
        {
            return lexical;
        }
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
            if *e != EOF {
                message.push_str("...");
            }
            message.push_str("\n    ");
        }
        invalid(message)
    }

    fn keyword(&mut self, keyword: &str, expected: &[&str]) -> Result<(), Status> {
        if self.is_keyword(keyword)? {
            self.next()?;
            Ok(())
        } else {
            Err(self.error(expected))
        }
    }

    fn punct(&mut self, punct: &str, expected: &[&str]) -> Result<(), Status> {
        if self.is_punct(punct)? {
            self.next()?;
            Ok(())
        } else {
            Err(self.error(expected))
        }
    }

    fn name(&mut self, expected: &[&str]) -> Result<String, Status> {
        match self.kind()? {
            Kind::Name | Kind::QuotedName => Ok(self.next()?.text),
            _ => Err(self.error(expected)),
        }
    }

    /// Refuses a name just read that `(` follows, in a select of entities: a function, which
    /// makes it an aggregation.
    fn not_function(&mut self) -> Result<(), Status> {
        if self.is_punct("(")? {
            return Err(invalid(ENTITY_QUERY_EXPECTED));
        }
        Ok(())
    }

    /// The rest of a property, its names joined with `.`.
    fn dotted(&mut self, mut name: String) -> Result<String, Status> {
        while self.is_punct(".")? {
            self.next()?;
            name.push('.');
            name.push_str(&self.name(NAME)?);
        }
        Ok(name)
    }

    /// `expected`, the end of the select (`<EOF>` first, or `)` after the keywords) put in.
    fn ending(
        within: Within,
        keywords: &[&'static str],
        rest: &[&'static str],
    ) -> Vec<&'static str> {
        let mut out = Vec::new();
        if within == Within::Top {
            out.push(EOF);
        }
        out.extend_from_slice(keywords);
        if within == Within::Over {
            out.push("\")\"");
        }
        out.extend_from_slice(rest);
        out
    }

    // --- queries ---------------------------------------------------------------------------------

    fn read(&mut self, expect: Expect) -> Result<Read, Status> {
        let read = match expect {
            Expect::Entities => {
                if self.is_keyword("aggregate")? {
                    return Err(invalid(ENTITY_QUERY_EXPECTED));
                }
                self.keyword("select", &["\"select\""])?;
                Read::Entities(self.select(Within::Top)?.query)
            }
            Expect::Aggregation => {
                if self.is_keyword("aggregate")? {
                    self.next()?;
                    Read::Aggregation(self.aggregate()?)
                } else {
                    self.keyword("select", &["\"aggregate\"", "\"select\""])?;
                    Read::Aggregation(self.select_aggregations()?)
                }
            }
        };
        Ok(read)
    }

    /// Whether the next tokens are a function and its `(`.
    fn function_ahead(&mut self) -> Result<bool, Status> {
        let first = matches!(
            self.kind()?,
            Kind::Keyword("avg" | "count" | "count_up_to" | "sum") | Kind::Name
        );
        Ok(first && matches!(self.look(1)?.kind, Kind::Punct("(")))
    }

    /// `AGGREGATE` functions `OVER (` a select `)`.
    fn aggregate(&mut self) -> Result<AggregationQuery, Status> {
        let mut aggregations = Vec::new();
        loop {
            let mut aggregation = self.function(false)?;
            if self.is_keyword("as")? {
                self.next()?;
                aggregation.alias = self.name(NAME)?;
                aggregations.push(aggregation);
                if self.is_punct(",")? {
                    self.next()?;
                    continue;
                }
                self.keyword("over", &["\"over\"", "\",\""])?;
            } else {
                aggregations.push(aggregation);
                if self.is_punct(",")? {
                    self.next()?;
                    continue;
                }
                self.keyword("over", &["\"as\"", "\"over\"", "\",\""])?;
            }
            break;
        }
        self.punct("(", &["\"(\""])?;
        self.keyword("select", &["\"select\""])?;
        let over = self.select(Within::Over)?;
        self.punct(")", &["\")\""])?;
        if self.kind()? != Kind::Eof {
            return Err(self.error(&[EOF]));
        }
        Ok(AggregationQuery {
            aggregations,
            query_type: Some(aggregation_query::QueryType::NestedQuery(over.query)),
        })
    }

    /// `SELECT` functions, then `FROM` and `WHERE`: an aggregation over a kind. A select of
    /// entities is refused as soon as it starts.
    fn select_aggregations(&mut self) -> Result<AggregationQuery, Status> {
        if !self.function_ahead()? {
            match self.kind()? {
                Kind::Keyword("distinct") | Kind::Punct("*") => {
                    self.next()?;
                }
                // A property is read whole first.
                Kind::Name | Kind::QuotedName => {
                    let name = self.next()?.text;
                    self.dotted(name)?;
                }
                _ => return Err(self.error(&["\"distinct\"", N, QN, "\"*\"", N])),
            }
            return Err(invalid(AGGREGATION_QUERY_EXPECTED));
        }
        let mut aggregations = Vec::new();
        loop {
            let mut aggregation = self.function(false)?;
            let aliased = self.is_keyword("as")?;
            if aliased {
                self.next()?;
                aggregation.alias = self.name(NAME)?;
            }
            aggregations.push(aggregation);
            if self.is_punct(",")? {
                self.next()?;
                continue;
            }
            let mut query = Query::default();
            if self.is_keyword("from")? {
                self.next()?;
                query.kind = vec![KindExpression {
                    name: self.name(NAME)?,
                }];
            } else if !self.is_keyword("where")? && self.kind()? != Kind::Eof {
                let expected: &[&str] = if aliased {
                    &[EOF, "\"from\"", "\"where\"", "\",\""]
                } else {
                    &[EOF, "\"as\"", "\"from\"", "\"where\"", "\",\""]
                };
                return Err(self.error(expected));
            }
            let mut dotted = false;
            if self.is_keyword("where")? {
                self.next()?;
                let (cond, ended) = self.or_cond()?;
                dotted = ended;
                query.filter = Some(filter(cond));
            } else if self.kind()? != Kind::Eof {
                return Err(self.error(&[EOF, "\"where\""]));
            }
            if self.kind()? != Kind::Eof {
                let expected: &[&str] = if dotted {
                    &[EOF, "\"and\"", "\"or\"", "\".\""]
                } else {
                    &[EOF, "\"and\"", "\"or\""]
                };
                return Err(self.error(expected));
            }
            return Ok(AggregationQuery {
                aggregations,
                query_type: Some(aggregation_query::QueryType::NestedQuery(query)),
            });
        }
    }

    /// An aggregation: a function, `(`, its argument, `)`. A function first in a select of
    /// entities (`entities_first`) that is not one makes the query an aggregation, refused.
    fn function(&mut self, entities_first: bool) -> Result<aggregation_query::Aggregation, Status> {
        use aggregation_query::aggregation::{Avg, Count, Operator, Sum};
        let token = self.peek()?.clone();
        let function = match &token.kind {
            Kind::Keyword(k @ ("avg" | "count" | "count_up_to" | "sum")) => (*k).to_owned(),
            Kind::Name => token.text.clone(),
            _ => return Err(self.error(FUNCTION)),
        };
        self.next()?;
        self.punct("(", &["\"(\""])?;
        let operator = match function.as_str() {
            "count" => {
                if self.is_punct("*")? {
                    self.next()?;
                    self.punct(")", &["\")\""])?;
                    Operator::Count(Count::default())
                } else {
                    // Refused once the property's first name is read.
                    self.name(&[N, QN, "\"*\""])?;
                    return Err(invalid(
                        "Cloud Datastore only supports counts over the entire result set (i.e. `COUNT(*)`)",
                    ));
                }
            }
            "count_up_to" => {
                if self.kind()? != Kind::Integer {
                    return Err(self.error(&[INT]));
                }
                let token = self.next()?;
                self.literal(&token.image)?;
                let up_to = parse_integer(&token.text)?;
                self.punct(")", &["\")\""])?;
                Operator::Count(Count { up_to: Some(up_to) })
            }
            "sum" | "avg" => {
                let name = self.name(NAME)?;
                let property = self.dotted(name)?;
                self.punct(")", &["\")\"", "\".\""])?;
                if function == "sum" {
                    Operator::Sum(Sum {
                        property: Some(reference(&property)),
                    })
                } else {
                    Operator::Avg(Avg {
                        property: Some(reference(&property)),
                    })
                }
            }
            _ if entities_first => return Err(invalid(ENTITY_QUERY_EXPECTED)),
            _ => {
                return Err(invalid(format!(
                    "Unexpected aggregation function: {function}"
                )));
            }
        };
        Ok(aggregation_query::Aggregation {
            alias: String::new(),
            operator: Some(operator),
        })
    }

    /// The rest of a select of entities, after `SELECT`, up to its end (the end of the text or
    /// the `)` of `OVER (…)`, which it leaves).
    fn select(&mut self, within: Within) -> Result<Select, Status> {
        let mut query = Query::default();
        // Functions here are an aggregation, which this query is not: refused once read.
        let mut start: &[&str] = &[
            "\"avg\"",
            "\"count\"",
            "\"count_up_to\"",
            "\"distinct\"",
            "\"sum\"",
            N,
            "\"*\"",
            N,
            N,
        ];
        let mut distinct = false;
        if self.is_keyword("distinct")? {
            self.next()?;
            start = &[
                "\"avg\"",
                "\"count\"",
                "\"count_up_to\"",
                "\"on\"",
                "\"sum\"",
                N,
                "\"*\"",
                N,
                N,
            ];
            if self.is_keyword("on")? {
                self.next()?;
                self.punct("(", &["\"(\""])?;
                let mut expected: &[&str] = &[N, QN, N];
                loop {
                    let quoted = self.kind()? == Kind::QuotedName;
                    let name = self.name(expected)?;
                    if !quoted {
                        self.not_function()?;
                    }
                    let property = self.dotted(name)?;
                    query.distinct_on.push(reference(&property));
                    if self.is_punct(",")? {
                        self.next()?;
                        expected = &[N, QN, N];
                        continue;
                    }
                    self.punct(")", &["\")\"", "\",\"", "\".\""])?;
                    break;
                }
                start = &[
                    "\"avg\"",
                    "\"count\"",
                    "\"count_up_to\"",
                    "\"sum\"",
                    N,
                    "\"*\"",
                    N,
                    N,
                ];
            } else {
                distinct = true;
            }
        }
        let named_function =
            self.kind()? == Kind::Name && matches!(self.look(1)?.kind, Kind::Punct("("));
        if named_function
            || matches!(
                self.kind()?,
                Kind::Keyword("avg" | "count" | "count_up_to" | "sum")
            )
        {
            let mut first = true;
            loop {
                self.function(first)?;
                first = false;
                if self.is_keyword("as")? {
                    self.next()?;
                    self.name(NAME)?;
                }
                if self.is_punct(",")? {
                    self.next()?;
                    continue;
                }
                return Err(invalid(ENTITY_QUERY_EXPECTED));
            }
        }
        let star = self.is_punct("*")?;
        let mut after: Vec<&'static str>;
        if star {
            self.next()?;
            after = Self::ending(
                within,
                &[
                    "\"from\"",
                    "\"group\"",
                    "\"limit\"",
                    "\"offset\"",
                    "\"order\"",
                    "\"where\"",
                ],
                &[],
            );
        } else {
            let mut expected: Vec<&str> = start.to_vec();
            let mut item = 0;
            loop {
                let quoted = self.kind()? == Kind::QuotedName;
                let name = self.name(&expected)?;
                if !quoted {
                    self.not_function()?;
                }
                // The first item's lookaheads list more, while it is one name.
                let property = if item == 0 && self.is_punct(".")? {
                    self.next()?;
                    let mut property = format!("{name}.{}", self.name(&[N, QN, N, QN, QN])?);
                    property = self.dotted(property)?;
                    item = 1;
                    property
                } else {
                    self.dotted(name)?
                };
                let first = item == 0;
                query.projection.push(Projection {
                    property: Some(reference(&property)),
                });
                if self.is_punct(",")? {
                    self.next()?;
                    expected = if item == 0 {
                        vec![N, QN, N, N, QN, N]
                    } else {
                        vec![N, QN, N]
                    };
                    item += 1;
                    continue;
                }
                let rest: &[&str] = match (first, quoted) {
                    (true, false) => &["\",\"", "\".\"", "\".\"", "\",\"", "\",\""],
                    (true, true) => &["\",\"", "\".\"", "\".\"", "\",\""],
                    (false, _) => &["\",\"", "\".\""],
                };
                after = Self::ending(
                    within,
                    &[
                        "\"from\"",
                        "\"group\"",
                        "\"limit\"",
                        "\"offset\"",
                        "\"order\"",
                        "\"where\"",
                    ],
                    rest,
                );
                break;
            }
            if distinct {
                query.distinct_on = query
                    .projection
                    .iter()
                    .filter_map(|p| p.property.clone())
                    .collect();
            }
        }
        if self.is_keyword("from")? {
            self.next()?;
            query.kind = vec![KindExpression {
                name: self.name(NAME)?,
            }];
            after = Self::ending(
                within,
                &[
                    "\"group\"",
                    "\"limit\"",
                    "\"offset\"",
                    "\"order\"",
                    "\"where\"",
                ],
                &[],
            );
        }
        if self.is_keyword("where")? {
            self.next()?;
            let (cond, dotted) = self.or_cond()?;
            query.filter = Some(filter(cond));
            after = Self::ending(
                within,
                &[
                    "\"and\"",
                    "\"group\"",
                    "\"limit\"",
                    "\"offset\"",
                    "\"or\"",
                    "\"order\"",
                ],
                if dotted { &["\".\""] } else { &[] },
            );
        }
        if self.is_keyword("group")? {
            self.next()?;
            self.keyword("by", &["\"by\""])?;
            return Err(invalid("GROUP BY not supported."));
        }
        if self.is_keyword("order")? {
            self.next()?;
            self.keyword("by", &["\"by\""])?;
            loop {
                let name = self.name(NAME)?;
                let property = self.dotted(name)?;
                let mut direction = property_order::Direction::Ascending;
                let mut rest: &[&str] = &["\",\"", "\".\""];
                let mut keywords: &[&str] = &["\"asc\"", "\"desc\"", "\"limit\"", "\"offset\""];
                if self.is_keyword("asc")? || self.is_keyword("desc")? {
                    if self.is_keyword("desc")? {
                        direction = property_order::Direction::Descending;
                    }
                    self.next()?;
                    keywords = &["\"limit\"", "\"offset\""];
                    rest = &["\",\""];
                }
                query.order.push(PropertyOrder {
                    property: Some(reference(&property)),
                    direction: direction as i32,
                });
                if self.is_punct(",")? {
                    self.next()?;
                    continue;
                }
                after = Self::ending(within, keywords, rest);
                break;
            }
        }
        if self.is_keyword("limit")? {
            self.next()?;
            self.limit(&mut query)?;
            after = Self::ending(within, &["\"offset\""], &[]);
        }
        if self.is_keyword("offset")? {
            self.next()?;
            let first = self.position("offset")?;
            let integer = first.is_ok();
            match first {
                Ok(n) => query.offset = bounded(n, "offset")?,
                Err(cursor) => query.start_cursor = cursor,
            }
            if self.is_punct("+")? {
                self.next()?;
                if integer {
                    return Err(invalid("Encountered \"+\" after integer offset."));
                }
                if let Ok(n) = self.position("offset")? {
                    query.offset = bounded(n, "offset")?;
                }
            } else {
                after = Self::ending(within, &[], &["\"+\""]);
            }
        }
        let ended = match within {
            Within::Top => self.kind()? == Kind::Eof,
            Within::Over => self.is_punct(")")?,
        };
        if !ended {
            return Err(self.error(&after));
        }
        Ok(Select { query })
    }

    /// `LIMIT`'s count (an integer, a binding), or `FIRST(` a count `,` a cursor `)`.
    fn limit(&mut self, query: &mut Query) -> Result<(), Status> {
        if self.kind()? == Kind::Name {
            let name = self.next()?.text;
            if !name.eq_ignore_ascii_case("first") {
                return Err(invalid(format!(
                    "Unexpected name \"{name}\" (after LIMIT)."
                )));
            }
            self.punct("(", &["\"(\""])?;
            let count = self.position("limit")?;
            if let Ok(n) = count {
                query.limit = Some(bounded(n, "limit")?);
            }
            self.punct(",", &["\",\""])?;
            let cursor_token = self.peek()?.clone();
            let cursor = self.position("limit")?;
            if cursor.is_ok() && cursor_token.kind != Kind::Integer && count.is_ok() {
                return Err(invalid(format!(
                    "Binding site {} for limit bound to non-cursor parameter.",
                    cursor_token.image
                )));
            }
            let cursor_image = cursor_token.image;
            match (count, cursor) {
                (Ok(_), Err(cursor)) => query.start_cursor = cursor,
                (Err(cursor), Ok(n)) => {
                    query.start_cursor = cursor;
                    query.limit = Some(bounded(n, "limit")?);
                }
                (_, _) => {
                    return Err(invalid(format!(
                        "Expected limit cursor instead of {cursor_image}."
                    )));
                }
            }
            self.punct(")", &["\")\""])?;
            return Ok(());
        }
        if !matches!(
            self.kind()?,
            Kind::Integer | Kind::NameBinding | Kind::PositionBinding
        ) {
            return Err(self.error(&[INT, N, NB, PB]));
        }
        match self.position("limit")? {
            Ok(n) => query.limit = Some(bounded(n, "limit")?),
            Err(cursor) => query.end_cursor = cursor,
        }
        Ok(())
    }

    /// An integer or a binding of `LIMIT` or `OFFSET` (`site`): `Ok` a number, `Err` a cursor.
    fn position(&mut self, site: &str) -> Result<Result<i64, Vec<u8>>, Status> {
        match self.kind()? {
            Kind::Integer => {
                let token = self.next()?;
                self.literal(&token.image)?;
                Ok(Ok(parse_integer(&token.text)?))
            }
            Kind::NameBinding | Kind::PositionBinding => {
                let token = self.next()?;
                let parameter = self.bindings.parameter(&token)?;
                match parameter.parameter_type.as_ref() {
                    Some(ParameterType::Cursor(cursor)) => Ok(Err(cursor.clone())),
                    Some(ParameterType::Value(Value {
                        value_type: Some(ValueType::IntegerValue(n)),
                        ..
                    })) => Ok(Ok(*n)),
                    Some(ParameterType::Value(_)) => Err(invalid(format!(
                        "Binding site {} for {site} bound to non-integer value parameter.",
                        token.image
                    ))),
                    None => Err(invalid("A GQL query parameter is empty.")),
                }
            }
            _ => Err(self.error(POSITION)),
        }
    }

    // --- conditions ------------------------------------------------------------------------------

    /// Conditions joined by `OR`; and whether the last one ends with a property.
    fn or_cond(&mut self) -> Result<(Cond, bool), Status> {
        let (first, mut dotted) = self.and_cond()?;
        let mut ors = vec![first];
        while self.is_keyword("or")? {
            self.next()?;
            let (next, ended) = self.and_cond()?;
            ors.push(next);
            dotted = ended;
        }
        Ok((
            if ors.len() == 1 {
                ors.pop().expect("one")
            } else {
                Cond::Or(ors)
            },
            dotted,
        ))
    }

    fn and_cond(&mut self) -> Result<(Cond, bool), Status> {
        let (first, mut dotted) = self.term()?;
        let mut ands = vec![first];
        while self.is_keyword("and")? {
            self.next()?;
            let (next, ended) = self.term()?;
            ands.push(next);
            dotted = ended;
        }
        Ok((
            if ands.len() == 1 {
                ands.pop().expect("one")
            } else {
                Cond::And(ands)
            },
            dotted,
        ))
    }

    fn value_start(kind: &Kind) -> bool {
        matches!(
            kind,
            Kind::Integer
                | Kind::Double
                | Kind::SingleQuoted
                | Kind::DoubleQuoted
                | Kind::NameBinding
                | Kind::PositionBinding
                | Kind::Keyword("true" | "false" | "null")
        )
    }

    /// A condition: in parentheses, a value first (a value and an operator, or a function and
    /// its `(`, ahead), or a property first.
    fn term(&mut self) -> Result<(Cond, bool), Status> {
        let first = self.kind()?;
        if first == Kind::Punct("(") {
            self.next()?;
            let (inner, dotted) = self.or_cond()?;
            let expected: &[&str] = if dotted {
                &["\"and\"", "\"or\"", "\")\"", "\".\""]
            } else {
                &["\"and\"", "\"or\"", "\")\""]
            };
            self.punct(")", expected)?;
            return Ok((inner, false));
        }
        let second = match first {
            Kind::Eof => Kind::Eof,
            _ => self.look(1)?.kind.clone(),
        };
        let value_first = (Self::value_start(&first)
            && matches!(
                second,
                Kind::Keyword("has" | "in") | Kind::Punct("=" | "!=" | "<" | "<=" | ">" | ">=")
            ))
            || (first == Kind::Name && second == Kind::Punct("("));
        if value_first {
            return self.value_first().map(|cond| (cond, true));
        }
        if matches!(first, Kind::Name | Kind::QuotedName) {
            return self.property_first().map(|cond| (cond, false));
        }
        if Self::value_start(&first) {
            return Err(self.error(&[N, QN, N, N, N]));
        }
        Err(self.error(&["\"(\""]))
    }

    /// A value, an operator, a property: `2 < a`, `'red' IN tags`, `KEY(…) HAS DESCENDANT
    /// __key__`.
    fn value_first(&mut self) -> Result<Cond, Status> {
        use property_filter::Operator as O;
        let value = self.value()?;
        let op = match self.kind()? {
            Kind::Keyword("in") => {
                self.next()?;
                O::In
            }
            Kind::Keyword("has") => {
                self.next()?;
                if self.is_keyword("child")? {
                    self.next()?;
                    self.refused.get_or_insert_with(|| {
                        invalid("The 'HAS_PARENT' filter is not supported.")
                    });
                    O::HasAncestor
                } else {
                    self.keyword("descendant", &["\"child\"", "\"descendant\""])?;
                    O::HasAncestor
                }
            }
            Kind::Punct(p @ ("=" | "!=" | "<" | "<=" | ">" | ">=")) => {
                self.next()?;
                match p {
                    "=" => O::Equal,
                    "!=" => O::NotEqual,
                    // A value first: the comparison turned around.
                    "<" => O::GreaterThan,
                    "<=" => O::GreaterThanOrEqual,
                    ">" => O::LessThan,
                    _ => O::LessThanOrEqual,
                }
            }
            _ => return Err(self.error(REVERSED_OPERATOR)),
        };
        let name = self.name(NAME)?;
        let property = self.dotted(name)?;
        Ok(Cond::Leaf {
            property,
            op: in_or_equal(op, &value),
            value,
        })
    }

    /// A property, an operator, a value.
    fn property_first(&mut self) -> Result<Cond, Status> {
        use property_filter::Operator as O;
        let name = self.name(NAME)?;
        let property = self.dotted(name)?;
        let op = match self.kind()? {
            // `CONTAINS` reads as `IN`: an equality unless the value is an array.
            Kind::Keyword("contains" | "in") => O::In,
            Kind::Keyword("has") => {
                self.next()?;
                if self.is_keyword("parent")? {
                    self.refused.get_or_insert_with(|| {
                        invalid("The 'HAS_PARENT' filter is not supported.")
                    });
                } else if !self.is_keyword("ancestor")? {
                    return Err(self.error(&["\"ancestor\"", "\"parent\""]));
                }
                self.next()?;
                let value = self.value()?;
                return Ok(Cond::Leaf {
                    property,
                    op: O::HasAncestor,
                    value,
                });
            }
            Kind::Keyword("not") => {
                self.next()?;
                if !(self.is_keyword("in")? || self.is_keyword("contains")?) {
                    return Err(self.error(&["\"contains\"", "\"in\""]));
                }
                O::NotIn
            }
            Kind::Keyword("is") => {
                self.next()?;
                if !self.is_keyword("null")? {
                    return Err(self.error(&["\"null\""]));
                }
                let token = self.next()?;
                self.literal(&token.image)?;
                return Ok(Cond::Leaf {
                    property,
                    op: O::Equal,
                    value: Value {
                        value_type: Some(ValueType::NullValue(0)),
                        ..Value::default()
                    },
                });
            }
            Kind::Punct("=") => O::Equal,
            Kind::Punct("!=") => O::NotEqual,
            Kind::Punct("<") => O::LessThan,
            Kind::Punct("<=") => O::LessThanOrEqual,
            Kind::Punct(">") => O::GreaterThan,
            Kind::Punct(">=") => O::GreaterThanOrEqual,
            _ => return Err(self.error(OPERATOR)),
        };
        self.next()?;
        let value = self.value()?;
        Ok(Cond::Leaf {
            property,
            op: in_or_equal(op, &value),
            value,
        })
    }

    // --- values ----------------------------------------------------------------------------------

    /// Refuses a literal (`image`, as written) where literals are not allowed.
    fn literal(&self, image: &str) -> Result<(), Status> {
        if self.bindings.allow_literals {
            Ok(())
        } else {
            Err(invalid(format!("Disallowed literal: {image}.")))
        }
    }

    /// A value, a literal's or a binding's, its keys' projects noted.
    fn value(&mut self) -> Result<Value, Status> {
        let value = self.value_read()?;
        if self.refused.is_none() && bad_project(&value) {
            self.bad_project_first = true;
        }
        Ok(value)
    }

    fn value_read(&mut self) -> Result<Value, Status> {
        let token = self.peek()?.clone();
        if !(Self::value_start(&token.kind) || token.kind == Kind::Name) {
            return Err(self.error(VALUE));
        }
        self.next()?;
        let value_type = match &token.kind {
            Kind::Keyword("true") => ValueType::BooleanValue(true),
            Kind::Keyword("false") => ValueType::BooleanValue(false),
            Kind::Keyword("null") => ValueType::NullValue(0),
            Kind::Integer => {
                self.literal(&token.image)?;
                ValueType::IntegerValue(parse_integer(&token.text)?)
            }
            Kind::Double => {
                self.literal(&token.image)?;
                ValueType::DoubleValue(parse_double(&token.text).ok_or_else(|| {
                    invalid(format!(
                        "Invalid double literal: For input string: \"{}\"",
                        token.text
                    ))
                })?)
            }
            Kind::SingleQuoted | Kind::DoubleQuoted => ValueType::StringValue(token.text.clone()),
            Kind::NameBinding | Kind::PositionBinding => {
                let parameter = self.bindings.parameter(&token)?;
                return match parameter.parameter_type.as_ref() {
                    Some(ParameterType::Value(value)) => Ok(value.clone()),
                    None => Err(invalid("A GQL query parameter is empty.")),
                    Some(ParameterType::Cursor(_)) => Err(invalid(format!(
                        "Binding site {} for value bound to non-value parameter.",
                        token.image
                    ))),
                };
            }
            _ => {
                let function = token.text.to_ascii_uppercase();
                if !matches!(function.as_str(), "KEY" | "ARRAY" | "BLOB" | "DATETIME") {
                    return Err(invalid(format!("Unknown function \"{}\".", token.text)));
                }
                self.literal(&token.image)?;
                return self.function_value(&function);
            }
        };
        if matches!(
            token.kind,
            Kind::Keyword(_) | Kind::SingleQuoted | Kind::DoubleQuoted
        ) {
            self.literal(&token.image)?;
        }
        Ok(Value {
            value_type: Some(value_type),
            ..Value::default()
        })
    }

    /// The rest of `KEY(…)`, `ARRAY(…)`, `BLOB('…')` or `DATETIME('…')`, after its name.
    fn function_value(&mut self, function: &str) -> Result<Value, Status> {
        self.punct("(", &["\"(\""])?;
        let value_type = match function {
            "KEY" => ValueType::KeyValue(self.key()?),
            "ARRAY" => {
                let mut values = Vec::new();
                if self.is_punct(")")? {
                    self.next()?;
                } else {
                    let mut expected: Vec<&str> = VALUE.to_vec();
                    expected.push("\")\"");
                    if !(Self::value_start(&self.kind()?) || self.kind()? == Kind::Name) {
                        return Err(self.error(&expected));
                    }
                    loop {
                        values.push(self.value()?);
                        if self.is_punct(",")? {
                            self.next()?;
                            continue;
                        }
                        self.punct(")", &["\")\"", "\",\""])?;
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
                self.punct(")", &["\")\""])?;
                ValueType::BlobValue(bytes)
            }
            "DATETIME" => {
                let text = self.string()?;
                let time = parse_datetime(&text).map_err(|why| datetime_refused(&text, why))?;
                self.punct(")", &["\")\""])?;
                ValueType::TimestampValue(time)
            }
            _ => unreachable!("a function value is one of these"),
        };
        Ok(Value {
            value_type: Some(value_type),
            ..Value::default()
        })
    }

    fn string(&mut self) -> Result<String, Status> {
        match self.kind()? {
            Kind::SingleQuoted | Kind::DoubleQuoted => Ok(self.next()?.text),
            _ => Err(self.error(&[SQ, DQ])),
        }
    }

    /// `KEY(`'s partition: `PROJECT('…')` then `NAMESPACE('…')`, each with its `,`, when they
    /// come; neither, the request's namespace.
    fn key(&mut self) -> Result<Key, Status> {
        let mut partition = PartitionId::default();
        let (mut project, mut namespace) = (false, false);
        loop {
            let function = match &self.peek()?.kind {
                Kind::Name => self.peek()?.text.to_ascii_lowercase(),
                _ => break,
            };
            if !matches!(function.as_str(), "project" | "namespace")
                || !matches!(self.look(1)?.kind, Kind::Punct("("))
            {
                break;
            }
            self.next()?;
            if function == "project" {
                if project {
                    return Err(invalid("A key may have at most one project."));
                }
                if namespace {
                    return Err(invalid("A key's project must precede its namespace."));
                }
                project = true;
            } else {
                if namespace {
                    return Err(invalid("A key may have at most one namespace."));
                }
                namespace = true;
            }
            self.next()?;
            let text = self.string()?;
            self.punct(")", &["\")\""])?;
            self.punct(",", &["\",\""])?;
            if function == "project" {
                partition.project_id = text;
            } else {
                partition.namespace_id = text;
            }
        }
        if !project && !namespace {
            partition.namespace_id = self.namespace.to_owned();
        }
        let mut path = Vec::new();
        let mut expected: &[&str] = &[N, QN, N];
        loop {
            let kind = self.name(expected)?;
            self.punct(",", &["\",\""])?;
            let id = match self.kind()? {
                Kind::Integer => IdType::Id(parse_integer(&self.next()?.text)?),
                Kind::SingleQuoted | Kind::DoubleQuoted => IdType::Name(self.next()?.text),
                _ => return Err(self.error(&[INT, SQ, DQ])),
            };
            path.push(PathElement {
                kind,
                id_type: Some(id),
            });
            if self.is_punct(",")? {
                self.next()?;
                expected = NAME;
                continue;
            }
            self.punct(")", &["\")\"", "\",\""])?;
            break;
        }
        Ok(Key {
            partition_id: Some(partition),
            path,
        })
    }
}

/// Whether `value` holds a key of an invalid project ([`keys::check_value_project`]).
fn bad_project(value: &Value) -> bool {
    match &value.value_type {
        Some(ValueType::KeyValue(key)) => keys::check_value_project(key).is_err(),
        Some(ValueType::ArrayValue(array)) => array.values.iter().any(bad_project),
        Some(ValueType::EntityValue(entity)) => {
            entity
                .key
                .as_ref()
                .is_some_and(|key| keys::check_value_project(key).is_err())
                || entity.properties.values().any(bad_project)
        }
        _ => false,
    }
}

/// Checks the keys of `filter`'s operands in the order they came, each as an operand's is: its
/// project, then its namespace and path.
fn check_keys(filter: Option<&Filter>) -> Result<(), Status> {
    fn value_keys(value: &Value) -> Result<(), Status> {
        match &value.value_type {
            Some(ValueType::KeyValue(key)) => {
                keys::check_value_project(key)?;
                keys::check_unpartitioned(key, keys::Rules::Read, keys::Last::Complete)?;
            }
            Some(ValueType::ArrayValue(array)) => {
                for element in &array.values {
                    value_keys(element)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    match filter.and_then(|f| f.filter_type.as_ref()) {
        Some(FilterType::CompositeFilter(composite)) => {
            for child in &composite.filters {
                check_keys(Some(child))?;
            }
        }
        Some(FilterType::PropertyFilter(property)) => {
            if let Some(value) = &property.value {
                value_keys(value)?;
            }
        }
        None => {}
    }
    Ok(())
}

/// `IN` a value that is not an array: an equality.
fn in_or_equal(op: property_filter::Operator, value: &Value) -> property_filter::Operator {
    if op == property_filter::Operator::In
        && !matches!(value.value_type, Some(ValueType::ArrayValue(_)))
    {
        property_filter::Operator::Equal
    } else {
        op
    }
}

/// A `LIMIT` or `OFFSET` count (`what`), refused below 0 or beyond 32 bits.
fn bounded(n: i64, what: &str) -> Result<i32, Status> {
    if n < 0 {
        return Err(invalid(format!("The {what} {n} is < 0.")));
    }
    i32::try_from(n).map_err(|_| invalid(format!("The {what} {n} is too large.")))
}

fn parse_integer(text: &str) -> Result<i64, Status> {
    text.strip_prefix('+').unwrap_or(text).parse().map_err(|_| {
        invalid(format!(
            "Invalid integer literal: For input string: \"{text}\""
        ))
    })
}

/// A double as Java reads one (`1.`, `.5`, `+.5`), or `None` (`1e`).
fn parse_double(text: &str) -> Option<f64> {
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
    text.parse().ok()
}

/// Why a `DATETIME` text does not read: as the official emulator words it, or an internal
/// error (a time a timestamp cannot hold).
fn datetime_refused(text: &str, reason: Option<&str>) -> Status {
    match reason {
        Some(reason) => invalid(format!("Invalid datetime text ({reason}): \"{text}\"")),
        None => Status::unknown(""),
    }
}

/// `YYYY-MM-DDThh:mm:ss[.S…]` (`t` too, up to six fractional digits) then `Z` (or `z`) or an
/// offset other than ±00:00, read as Java's `GregorianCalendar` reads it, strictly: the date is
/// Julian before 15 October 1582, the ten days before that do not exist, and the offset is
/// from -13:00 to +14:00. `Err` the reason it does not read (`None`: beyond a timestamp).
fn parse_datetime(text: &str) -> Result<Timestamp, Option<&'static str>> {
    const PATTERN: Option<&str> = Some("does not match pattern");
    const PARSE: Option<&str> = Some("parse failure");
    let number = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let field = |range: std::ops::Range<usize>| text.get(range).and_then(number);
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(PATTERN);
    }
    let [
        Some(year),
        Some(month),
        Some(day),
        Some(hour),
        Some(minute),
        Some(second),
    ] = [0..4, 5..7, 8..10, 11..13, 14..16, 17..19].map(field)
    else {
        return Err(PATTERN);
    };
    let mut rest = &text[19..];
    let mut nanos = 0i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let n = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 || n > 6 {
            return Err(PATTERN);
        }
        nanos = number(&format!("{:0<9}", &fraction[..n])).ok_or(PATTERN)?;
        rest = &fraction[n..];
    }
    let offset = if rest == "Z" || rest == "z" {
        0
    } else {
        let sign = match rest.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return Err(PATTERN),
        };
        if rest.len() != 6 || rest.as_bytes()[3] != b':' {
            return Err(PATTERN);
        }
        let (Some(h), Some(m)) = (number(&rest[1..3]), number(&rest[4..6])) else {
            return Err(PATTERN);
        };
        if h == 0 && m == 0 {
            return Err(Some("zero offset must be Z"));
        }
        let offset = sign * (h * 3600 + m * 60);
        if m > 59 || !(-13 * 3600..=14 * 3600).contains(&offset) {
            return Err(PARSE);
        }
        offset
    };
    // The Julian calendar up to 4 October 1582, the Gregorian from 15 October.
    let julian = (year, month, day) < (1582, 10, 15);
    if (year, month) == (1582, 10) && (5..15).contains(&day) {
        return Err(PARSE);
    }
    let leap = if julian {
        year % 4 == 0
    } else {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    };
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(PARSE),
    };
    if year < 1 || !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return Err(PARSE);
    }
    let days = if julian {
        days_from_julian(year, month, day)
    } else {
        days_from_civil(year, month, day)
    };
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    // 0001-01-01T00:00:00Z to 9999-12-31T23:59:59Z.
    if !(-62_135_596_800..=253_402_300_799).contains(&seconds) {
        return Err(None);
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(Timestamp {
        seconds,
        nanos: nanos as i32,
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

/// The days from 1970-01-01 to a date of the Julian calendar, by its Julian day number.
fn days_from_julian(year: i64, month: i64, day: i64) -> i64 {
    let a = (14 - month) / 12;
    let y = year + 4800 - a;
    let m = month + 12 * a - 3;
    day + (153 * m + 2) / 5 + 365 * y + y / 4 - 32_083 - 2_440_588
}

// --- bindings ----------------------------------------------------------------------------------

struct Bindings<'a> {
    named: HashMap<&'a str, &'a GqlQueryParameter>,
    positional: &'a [GqlQueryParameter],
    used: Vec<bool>,
    allow_literals: bool,
}

impl<'a> Bindings<'a> {
    /// The parameter a binding site names: refused when there is none.
    fn parameter(&mut self, site: &Token) -> Result<&'a GqlQueryParameter, Status> {
        if site.kind == Kind::NameBinding {
            return self
                .named
                .get(site.text.as_str())
                .copied()
                .ok_or_else(|| invalid(format!("Absent named binding: {}.", site.text)));
        }
        let n: usize = site.text.parse().unwrap_or(0);
        let parameter = n
            .checked_sub(1)
            .and_then(|i| self.positional.get(i))
            .ok_or_else(|| invalid(format!("Absent positional binding: {n}.")))?;
        self.used[n - 1] = true;
        Ok(parameter)
    }
}

fn reference(name: &str) -> PropertyReference {
    PropertyReference {
        name: name.to_owned(),
    }
}

fn filter(cond: Cond) -> Filter {
    let filter_type = match cond {
        Cond::And(children) => FilterType::CompositeFilter(CompositeFilter {
            op: composite_filter::Operator::And as i32,
            filters: children.into_iter().map(filter).collect(),
        }),
        Cond::Or(children) => FilterType::CompositeFilter(CompositeFilter {
            op: composite_filter::Operator::Or as i32,
            filters: children.into_iter().map(filter).collect(),
        }),
        Cond::Leaf {
            property,
            op,
            value,
        } => FilterType::PropertyFilter(PropertyFilter {
            property: Some(reference(&property)),
            op: op as i32,
            value: Some(value),
        }),
    };
    Filter {
        filter_type: Some(filter_type),
    }
}

/// A named binding's name: not empty, of name characters, not reserved, not a number's start.
fn binding_name(name: &str) -> Result<(), Status> {
    if name.is_empty() {
        return Err(invalid(
            "The GQL query parameter name is empty. is the empty string.",
        ));
    }
    if !name.chars().all(name_char) {
        return Err(invalid(format!(
            "GQL query parameter name \"{name}\" contains an invalid character."
        )));
    }
    if super::keys::reserved(name) {
        return Err(invalid(format!(
            "The GQL query parameter name \"{name}\" is reserved."
        )));
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(invalid(format!(
            "GQL query parameter name \"{name}\" starts with a digit."
        )));
    }
    Ok(())
}

/// What a GQL query reads as.
pub enum Read {
    Entities(Query),
    Aggregation(AggregationQuery),
}

/// Reads `gql` as `expect` says: its bindings checked, its text parsed and translated.
pub fn read(gql: &GqlQuery, namespace: &str, expect: Expect) -> Result<Read, Status> {
    // Each named binding's name, then its parameter, then the positional ones'. The official
    // emulator takes the named ones in the order they came; hidane by name.
    let mut named: Vec<(&String, &GqlQueryParameter)> = gql.named_bindings.iter().collect();
    named.sort_by(|a, b| a.0.cmp(b.0));
    for (name, parameter) in named {
        binding_name(name)?;
        if parameter.parameter_type.is_none() {
            return Err(invalid("A GQL query parameter is empty."));
        }
    }
    if gql
        .positional_bindings
        .iter()
        .any(|p| p.parameter_type.is_none())
    {
        return Err(invalid("A GQL query parameter is empty."));
    }
    let mut parser = Parser {
        lexer: Lexer {
            chars: gql.query_string.chars().collect(),
            pos: 0,
            line: 1,
            column: 0,
        },
        tokens: Vec::new(),
        lexical: None,
        at: 0,
        bindings: Bindings {
            named: gql
                .named_bindings
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect(),
            positional: &gql.positional_bindings,
            used: vec![false; gql.positional_bindings.len()],
            allow_literals: gql.allow_literals,
        },
        refused: None,
        bad_project_first: false,
        namespace,
    };
    let read = parser.read(expect)?;
    match &read {
        // An aggregation never tells of an unused positional binding; a key of an invalid
        // project is an internal error, unless a refusal came first.
        Read::Aggregation(_) => {
            if parser.bad_project_first {
                return Err(Status::unknown(""));
            }
        }
        // An entity query tells of an unused positional binding, then of its keys, in the
        // order they came, then of a refusal.
        Read::Entities(query) => {
            if let Some(unused) = parser.bindings.used.iter().position(|used| !used) {
                return Err(invalid(format!(
                    "Unused positional binding: {}.",
                    unused + 1
                )));
            }
            check_keys(query.filter.as_ref())?;
        }
    }
    if let Some(refused) = parser.refused {
        return Err(refused);
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gql(text: &str) -> Result<Read, Status> {
        read(
            &GqlQuery {
                query_string: text.into(),
                allow_literals: true,
                ..GqlQuery::default()
            },
            "",
            Expect::Entities,
        )
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
    fn datetimes_read_as_javas_calendar_reads_them() {
        let ts = parse_datetime("2021-06-01T21:00:00.500000+09:00").unwrap();
        assert_eq!(ts.seconds, 1_622_548_800);
        assert_eq!(ts.nanos, 500_000_000);
        assert_eq!(
            parse_datetime("2020-01-01").unwrap_err(),
            Some("does not match pattern")
        );
        assert_eq!(
            parse_datetime("2020-01-01T00:00:00+00:00").unwrap_err(),
            Some("zero offset must be Z")
        );
        assert_eq!(
            parse_datetime("2019-02-29T00:00:00Z").unwrap_err(),
            Some("parse failure")
        );
        // Julian before the Gregorian calendar: 4 October 1582 is the day before 15 October.
        let julian = parse_datetime("1582-10-04t00:00:00z").unwrap();
        let gregorian = parse_datetime("1582-10-15T00:00:00Z").unwrap();
        assert_eq!(gregorian.seconds - julian.seconds, 86_400);
        assert_eq!(
            parse_datetime("1582-10-10T00:00:00Z").unwrap_err(),
            Some("parse failure")
        );
        assert_eq!(parse_datetime("0001-01-01T00:00:00Z").unwrap_err(), None);
    }
}
