//! Evaluator A: lexer and parser for Rules conditions (CEL expressions plus `is`, path
//! literals and slices). Every node keeps its byte offset for diagnostics.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(u64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Ident(String),
    /// `/segment` of a path literal.
    PathSeg(String),
    /// `/$(` of a path literal.
    PathExpr,
    Punct(&'static str),
    Eof,
}

// `--` and `++` are tokens of the Rules grammar that no expression accepts: `--1` is an error.
const PUNCT: [&str; 24] = [
    "==", "!=", "<=", ">=", "&&", "||", "--", "++", "(", ")", "[", "]", "{", "}", ".", ",", ":",
    "?", "!", "-", "+", "*", "/", "%",
];

pub fn lex(src: &str) -> Result<Vec<(Tok, usize)>, String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if src[i..].starts_with("//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < b.len() && (b[i + 1] == b'$' || is_seg(b[i + 1])) {
            if src[i + 1..].starts_with("$(") {
                out.push((Tok::PathExpr, i));
                i += 3;
            } else {
                let start = i + 1;
                let mut j = start;
                let mut depth = 0;
                while j < b.len() && (is_seg(b[j]) || (b[j] == b')' && depth > 0)) {
                    depth += (b[j] == b'(') as i32 - (b[j] == b')') as i32;
                    j += 1;
                }
                out.push((Tok::PathSeg(src[start..j].to_string()), i));
                i = j;
            }
        } else if c.is_ascii_digit()
            || (c == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() && !after_operand(&out))
        {
            let (t, n) = number(&src[i..]).map_err(|e| format!("{e} at {i}"))?;
            out.push((t, i));
            i += n;
        } else if c == b'\''
            || c == b'"'
            || ((c == b'b' || c == b'B') && matches!(b.get(i + 1), Some(b'\'' | b'"')))
        {
            let (t, n) = string(&src[i..]).map_err(|e| format!("{e} at {i}"))?;
            out.push((t, i));
            i += n;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            out.push((Tok::Ident(src[i..j].to_string()), i));
            i = j;
        } else if let Some(p) = PUNCT.iter().find(|p| src[i..].starts_with(**p)) {
            out.push((Tok::Punct(p), i));
            i += p.len();
        } else if c == b'<' || c == b'>' {
            out.push((Tok::Punct(if c == b'<' { "<" } else { ">" }), i));
            i += 1;
        } else {
            return Err(format!("unexpected character at {i}"));
        }
    }
    out.push((Tok::Eof, src.len()));
    Ok(out)
}

fn is_seg(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_-.~%(".contains(&c)
}

fn after_operand(out: &[(Tok, usize)]) -> bool {
    matches!(
        out.last(),
        Some((
            Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Punct(")" | "]" | "}"),
            _
        ))
    )
}

/// Rules numbers are decimal, without exponents: `1e3` and `0x10` do not compile.
fn number(s: &str) -> Result<(Tok, usize), String> {
    let b = s.as_bytes();
    let mut n = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut float = false;
    if n < b.len() && b[n] == b'.' && n + 1 < b.len() && b[n + 1].is_ascii_digit() {
        float = true;
        n += 1;
        n += b[n..].iter().take_while(|c| c.is_ascii_digit()).count();
    }
    if n < b.len() && (b[n].is_ascii_alphabetic() || b[n] == b'_') {
        return Err(format!("unexpected '{}' after a number", b[n] as char));
    }
    if float {
        Ok((
            Tok::Float(s[..n].parse().map_err(|_| "bad float literal")?),
            n,
        ))
    } else {
        Ok((
            Tok::Int(s[..n].parse().map_err(|_| "integer literal out of range")?),
            n,
        ))
    }
}

/// Rules strings: single or double quotes, an optional `b` prefix for bytes, no raw or
/// triple-quoted forms, no unescaped line breaks, backspace or form feed (other control
/// characters are fine), and only the escapes `\n \r \t \b \f \\ \' \" \xHH \uHHHH` and
/// three-digit octal.
fn string(s: &str) -> Result<(Tok, usize), String> {
    let b = s.as_bytes();
    let bytes = b[0] == b'b' || b[0] == b'B';
    let mut i = bytes as usize;
    let q = b[i];
    i += 1;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let Some(&c) = b.get(i) else {
            return Err("unterminated string".into());
        };
        if c == q {
            i += 1;
            break;
        }
        if matches!(c, b'\n' | b'\r' | 0x08 | 0x0c) {
            return Err("control character in string".into());
        }
        if c == b'\\' {
            let e = *b.get(i + 1).ok_or("unterminated escape")?;
            i += 2;
            let simple = match e {
                b'n' => Some(10),
                b'b' => Some(8),
                b'f' => Some(12),
                b'r' => Some(13),
                b't' => Some(9),
                b'\\' | b'\'' | b'"' => Some(e),
                _ => None,
            };
            if let Some(v) = simple {
                out.push(v);
                continue;
            }
            let (code, n) = match e {
                b'0'..=b'3' => (
                    u32::from_str_radix(s.get(i - 1..i + 2).ok_or("bad octal")?, 8)
                        .map_err(|_| "bad octal escape")?,
                    2,
                ),
                b'x' => (
                    u32::from_str_radix(s.get(i..i + 2).ok_or("bad hex")?, 16)
                        .map_err(|_| "bad hex escape")?,
                    2,
                ),
                b'u' if !bytes => (
                    u32::from_str_radix(s.get(i..i + 4).ok_or("bad \\u")?, 16)
                        .map_err(|_| "bad \\u escape")?,
                    4,
                ),
                _ => return Err(format!("invalid escape \\{}", e as char)),
            };
            i += n;
            if bytes {
                out.push(code as u8);
            } else {
                let ch = char::from_u32(code).ok_or("invalid code point")?;
                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    if bytes {
        Ok((Tok::Bytes(out), i))
    } else {
        Ok((
            Tok::Str(String::from_utf8(out).map_err(|_| "invalid UTF-8 in string")?),
            i,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub pos: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Ident(String),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    /// A path literal: literal segments and `$(expr)` segments.
    Path(Vec<Result<String, Expr>>),
    Select(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Slice(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Global call (`f(x)`, `math.abs(x)`) or method call (`x.f(y)`).
    Call(Option<Box<Expr>>, String, Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Is(Box<Expr>, String),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

pub struct Parser {
    toks: Vec<(Tok, usize)>,
    i: usize,
    depth: usize,
}

pub fn parse(src: &str) -> Result<Expr, String> {
    let mut p = Parser {
        toks: lex(src)?,
        i: 0,
        depth: 0,
    };
    let e = p.expr()?;
    match p.peek() {
        Tok::Eof => Ok(e),
        t => Err(format!("unexpected {t:?} at {}", p.pos())),
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.i].0
    }
    fn pos(&self) -> usize {
        self.toks[self.i].1
    }
    fn next(&mut self) -> Tok {
        let t = self.toks[self.i].0.clone();
        if t != Tok::Eof {
            self.i += 1;
        }
        t
    }
    fn eat(&mut self, p: &str) -> bool {
        if matches!(self.peek(), Tok::Punct(q) if *q == p) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: &str) -> Result<(), String> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(format!(
                "expected '{p}' at {}, found {:?}",
                self.pos(),
                self.peek()
            ))
        }
    }
    fn node(pos: usize, kind: Kind) -> Expr {
        Expr { pos, kind }
    }

    pub fn expr(&mut self) -> Result<Expr, String> {
        self.depth += 1;
        if self.depth > 250 {
            return Err("expression nested too deeply".into());
        }
        let pos = self.pos();
        let c = self.or()?;
        let e = if self.eat("?") {
            let a = self.or()?;
            self.expect(":")?;
            let b = self.expr()?;
            Self::node(pos, Kind::Cond(Box::new(c), Box::new(a), Box::new(b)))
        } else {
            c
        };
        self.depth -= 1;
        Ok(e)
    }

    fn binary(
        &mut self,
        ops: &[&'static str],
        next: fn(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        let pos = self.pos();
        let mut l = next(self)?;
        'outer: loop {
            for op in ops {
                if self.eat(op) {
                    let r = next(self)?;
                    l = Self::node(pos, Kind::Binary(op, Box::new(l), Box::new(r)));
                    continue 'outer;
                }
            }
            return Ok(l);
        }
    }
    fn or(&mut self) -> Result<Expr, String> {
        self.binary(&["||"], Self::and)
    }
    fn and(&mut self) -> Result<Expr, String> {
        self.binary(&["&&"], Self::relation)
    }
    fn relation(&mut self) -> Result<Expr, String> {
        let pos = self.pos();
        let mut l = self.additive()?;
        loop {
            if let Tok::Ident(w) = self.peek() {
                if w == "in" {
                    self.i += 1;
                    let r = self.additive()?;
                    l = Self::node(pos, Kind::Binary("in", Box::new(l), Box::new(r)));
                    continue;
                }
                if w == "is" {
                    self.i += 1;
                    let Tok::Ident(t) = self.next() else {
                        return Err(format!("expected a type name at {}", self.pos()));
                    };
                    l = Self::node(pos, Kind::Is(Box::new(l), t));
                    continue;
                }
            }
            let op = ["==", "!=", "<=", ">=", "<", ">"]
                .into_iter()
                .find(|op| self.eat(op));
            match op {
                Some(op) => {
                    let r = self.additive()?;
                    l = Self::node(pos, Kind::Binary(op, Box::new(l), Box::new(r)));
                }
                None => return Ok(l),
            }
        }
    }
    fn additive(&mut self) -> Result<Expr, String> {
        self.binary(&["+", "-"], Self::multiplicative)
    }
    fn multiplicative(&mut self) -> Result<Expr, String> {
        self.binary(&["*", "/", "%"], Self::unary)
    }
    fn unary(&mut self) -> Result<Expr, String> {
        let pos = self.pos();
        if self.eat("!") {
            return Ok(Self::node(pos, Kind::Unary("!", Box::new(self.unary()?))));
        }
        if self.eat("-") {
            // -9223372036854775808 is a literal, not the negation of an out-of-range one.
            if let Tok::Int(v) = *self.peek()
                && v == 1 << 63
                && !self.postfix_follows()
            {
                self.i += 1;
                return Ok(Self::node(pos, Kind::Int(i64::MIN)));
            }
            return Ok(Self::node(pos, Kind::Unary("-", Box::new(self.unary()?))));
        }
        self.postfix()
    }
    fn postfix_follows(&self) -> bool {
        matches!(self.toks.get(self.i + 1), Some((Tok::Punct("." | "["), _)))
    }
    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            let pos = self.pos();
            if self.eat(".") {
                let Tok::Ident(name) = self.next() else {
                    return Err(format!("expected a field name at {pos}"));
                };
                if self.eat("(") {
                    let args = self.args(")")?;
                    e = Self::node(pos, Kind::Call(Some(Box::new(e)), name, args));
                } else {
                    e = Self::node(pos, Kind::Select(Box::new(e), name));
                }
            } else if self.eat("[") {
                let i = self.expr()?;
                if self.eat(":") {
                    let j = self.expr()?;
                    self.expect("]")?;
                    e = Self::node(pos, Kind::Slice(Box::new(e), Box::new(i), Box::new(j)));
                } else {
                    self.expect("]")?;
                    e = Self::node(pos, Kind::Index(Box::new(e), Box::new(i)));
                }
            } else {
                return Ok(e);
            }
        }
    }
    fn args(&mut self, close: &str) -> Result<Vec<Expr>, String> {
        let mut out = Vec::new();
        if self.eat(close) {
            return Ok(out);
        }
        loop {
            out.push(self.expr()?);
            if self.eat(close) {
                return Ok(out);
            }
            self.expect(",")?;
            if self.eat(close) {
                return Ok(out);
            }
        }
    }
    fn primary(&mut self) -> Result<Expr, String> {
        let pos = self.pos();
        let kind = match self.next() {
            Tok::Int(v) => Kind::Int(
                i64::try_from(v).map_err(|_| format!("integer literal out of range at {pos}"))?,
            ),
            Tok::Float(v) => Kind::Float(v),
            Tok::Str(s) => Kind::Str(s),
            Tok::Bytes(b) => Kind::Bytes(b),
            Tok::Ident(w) => match w.as_str() {
                "true" => Kind::Bool(true),
                "false" => Kind::Bool(false),
                "null" => Kind::Null,
                _ if self.eat("(") => Kind::Call(None, w, self.args(")")?),
                // math.abs(x), duration.value(...): a namespaced global function.
                "math" | "duration" | "timestamp" | "latlng" | "hashing"
                    if matches!(self.peek(), Tok::Punct(".")) =>
                {
                    self.i += 1;
                    let Tok::Ident(f) = self.next() else {
                        return Err(format!("expected a function name at {pos}"));
                    };
                    self.expect("(")?;
                    Kind::Call(None, format!("{w}.{f}"), self.args(")")?)
                }
                _ => Kind::Ident(w),
            },
            Tok::Punct("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                return Ok(e);
            }
            Tok::Punct("[") => Kind::List(self.args("]")?),
            Tok::Punct("{") => {
                let mut entries = Vec::new();
                if !self.eat("}") {
                    loop {
                        let k = self.expr()?;
                        self.expect(":")?;
                        entries.push((k, self.expr()?));
                        if self.eat("}") {
                            break;
                        }
                        self.expect(",")?;
                        if self.eat("}") {
                            break;
                        }
                    }
                }
                Kind::Map(entries)
            }
            t @ (Tok::PathSeg(_) | Tok::PathExpr) => {
                let mut segs = Vec::new();
                let mut t = t;
                loop {
                    match t {
                        Tok::PathSeg(s) => segs.push(Ok(s)),
                        Tok::PathExpr => {
                            segs.push(Err(self.expr()?));
                            self.expect(")")?;
                        }
                        _ => unreachable!(),
                    }
                    if matches!(self.peek(), Tok::PathSeg(_) | Tok::PathExpr) {
                        t = self.next();
                    } else {
                        break;
                    }
                }
                Kind::Path(segs)
            }
            t => return Err(format!("unexpected {t:?} at {pos}")),
        };
        Ok(Self::node(pos, kind))
    }
}
