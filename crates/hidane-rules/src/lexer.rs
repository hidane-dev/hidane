//! The lexer, with the official compiler's lexical rules as measured (#59, #61).
//!
//! - Whitespace is space, tab, CR, LF and form feed; `//` and `/* */` are comments. Any other
//!   character that starts no token (a non-breaking space, `#`, a non-ASCII letter) is a
//!   "token recognition error".
//! - A path segment is one token: `/` followed by `a-z A-Z 0-9 - . _ ~ & ' * + : @` or `%HH`, or
//!   by such characters in parentheses (`/(default)`). So `4/2` is `4` and the path `/2`, as on
//!   the official emulator. A comment, when longer, wins (`/* … */`).
//! - Numbers are decimal: `1`, `1.5`, `1.`, `.5`; no exponents, no hex.
//! - Identifiers may contain `$` (`$b`); a lone `$` is a token (`/$(…)`), and so is `@`.
//! - The keywords include CEL's reserved `break continue else for package var while`.
//! - Strings are quoted with `'` or `"`; the escapes are `\n \r \t \b \f \\ \' \"`, `\xHH`,
//!   `\uHHHH` and three octal digits; a raw line break, CR, backspace or form feed ends nothing:
//!   the string is then unpaired (`'abc` without its closing quote), a token of its own.
//!   `b'…'` is bytes.
//! - `--` and `++` are tokens (no expression accepts them).
//!
//! Positions are byte offsets into the source, plus the line and column (1-based, in code
//! points) that diagnostics print.

/// The kinds of token. Punctuation is named after what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Tok {
    AndAnd,
    /// A token of the official grammar that no rule here accepts.
    At,
    Colon,
    Comma,
    Dec,
    Dollar,
    Dot,
    EqEq,
    Assign,
    Gt,
    Ge,
    Inc,
    LBrace,
    LBracket,
    LParen,
    Lt,
    Le,
    Minus,
    Percent,
    Ne,
    Not,
    OrOr,
    Plus,
    Question,
    RBrace,
    RBracket,
    RParen,
    Semi,
    Slash,
    Star,
    Allow,
    False,
    Function,
    If,
    Import,
    In,
    Is,
    Let,
    Match,
    Null,
    Return,
    RulesVersion,
    Service,
    True,
    /// CEL's reserved words the official grammar also reserves: no variable, function or
    /// method can have these names, a field can.
    Break,
    Continue,
    Else,
    For,
    Package,
    Var,
    While,
    PathSegment,
    Ident,
    Int,
    Float,
    String,
    Bytes,
    UnpairedSingle,
    UnpairedDouble,
    Eof,
}

impl Tok {
    /// How ANTLR names the token in "expecting …".
    pub(crate) fn display(self) -> &'static str {
        match self {
            Self::AndAnd => "'&&'",
            Self::At => "'@'",
            Self::Colon => "':'",
            Self::Comma => "','",
            Self::Dec => "'--'",
            Self::Dollar => "'$'",
            Self::Dot => "'.'",
            Self::EqEq => "'=='",
            Self::Assign => "'='",
            Self::Gt => "'>'",
            Self::Ge => "'>='",
            Self::Inc => "'++'",
            Self::LBrace => "'{'",
            Self::LBracket => "'['",
            Self::LParen => "'('",
            Self::Lt => "'<'",
            Self::Le => "'<='",
            Self::Minus => "'-'",
            Self::Percent => "'%'",
            Self::Ne => "'!='",
            Self::Not => "'!'",
            Self::OrOr => "'||'",
            Self::Plus => "'+'",
            Self::Question => "'?'",
            Self::RBrace => "'}'",
            Self::RBracket => "']'",
            Self::RParen => "')'",
            Self::Semi => "';'",
            Self::Slash => "'/'",
            Self::Star => "'*'",
            Self::Allow => "'allow'",
            Self::False => "'false'",
            Self::Function => "'function'",
            Self::If => "'if'",
            Self::Import => "'import'",
            Self::In => "'in'",
            Self::Is => "'is'",
            Self::Let => "'let'",
            Self::Match => "'match'",
            Self::Null => "'null'",
            Self::Return => "'return'",
            Self::RulesVersion => "'rules_version'",
            Self::Service => "'service'",
            Self::True => "'true'",
            Self::Break => "'break'",
            Self::Continue => "'continue'",
            Self::Else => "'else'",
            Self::For => "'for'",
            Self::Package => "'package'",
            Self::Var => "'var'",
            Self::While => "'while'",
            Self::PathSegment => "PATH_SEGMENT",
            Self::Ident => "IDENTIFIER",
            Self::Int => "NUM_INT",
            Self::Float => "NUM_FLOAT",
            Self::String => "STRING",
            Self::Bytes => "BYTES",
            Self::UnpairedSingle => "UNPAIRED_SINGLE_QUOTE",
            Self::UnpairedDouble => "UNPAIRED_DOUBLE_QUOTE",
            Self::Eof => "<EOF>",
        }
    }

    /// Whether the token is a word: an identifier or a keyword.
    pub(crate) fn is_word(self) -> bool {
        matches!(
            self,
            Self::Ident
                | Self::Allow
                | Self::False
                | Self::Function
                | Self::If
                | Self::Import
                | Self::In
                | Self::Is
                | Self::Let
                | Self::Match
                | Self::Null
                | Self::Return
                | Self::RulesVersion
                | Self::Service
                | Self::True
                | Self::Break
                | Self::Continue
                | Self::Else
                | Self::For
                | Self::Package
                | Self::Var
                | Self::While
        )
    }

    fn keyword(word: &str) -> Option<Self> {
        Some(match word {
            "allow" => Self::Allow,
            "false" => Self::False,
            "function" => Self::Function,
            "if" => Self::If,
            "import" => Self::Import,
            "in" => Self::In,
            "is" => Self::Is,
            "let" => Self::Let,
            "match" => Self::Match,
            "null" => Self::Null,
            "return" => Self::Return,
            "rules_version" => Self::RulesVersion,
            "service" => Self::Service,
            "true" => Self::True,
            "break" => Self::Break,
            "continue" => Self::Continue,
            "else" => Self::Else,
            "for" => Self::For,
            "package" => Self::Package,
            "var" => Self::Var,
            "while" => Self::While,
            _ => return None,
        })
    }
}

/// A token: its kind and where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: Tok,
    /// Byte offsets, end exclusive.
    pub start: usize,
    pub end: usize,
    pub line: u32,
    /// 1-based, in code points.
    pub column: u32,
}

/// A character that starts no token, as ANTLR reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LexError {
    pub start: usize,
    pub line: u32,
    pub column: u32,
    /// What the lexer could not match, with `\n`, `\r` and `\t` spelled out.
    pub text: String,
}

pub(crate) struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
}

pub(crate) fn lex(src: &str) -> Lexed {
    let mut lexer = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        line: 1,
        column: 1,
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    lexer.run();
    Lexed {
        tokens: lexer.tokens,
        errors: lexer.errors,
    }
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    column: u32,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c') {
                self.advance(1);
                continue;
            }
            if c == b'/' {
                let comment = self.comment_len();
                let segment = self.path_segment_len();
                if comment > 0 && comment >= segment {
                    self.advance(comment);
                } else if segment > 0 {
                    self.push(Tok::PathSegment, segment);
                } else {
                    self.push(Tok::Slash, 1);
                }
                continue;
            }
            if c.is_ascii_digit() || (c == b'.' && self.peek(1).is_some_and(|d| d.is_ascii_digit()))
            {
                let len = self.number_len();
                let kind = if self.bytes[self.pos..self.pos + len].contains(&b'.') {
                    Tok::Float
                } else {
                    Tok::Int
                };
                self.push(kind, len);
                continue;
            }
            if (c == b'b' || c == b'B') && matches!(self.peek(1), Some(b'\'' | b'"')) {
                let (len, closed) = string_len(&self.bytes[self.pos + 1..]);
                if closed {
                    self.push(Tok::Bytes, len + 1);
                    continue;
                }
                // No bytes literal: `b` is an identifier, the quote starts what follows.
            }
            if c == b'\'' || c == b'"' {
                let (len, closed) = string_len(&self.bytes[self.pos..]);
                let kind = match (closed, c) {
                    (true, _) => Tok::String,
                    (false, b'\'') => Tok::UnpairedSingle,
                    (false, _) => Tok::UnpairedDouble,
                };
                self.push(kind, len);
                continue;
            }
            // Identifiers may contain `$`; a lone `$` is the token of `$(…)`.
            let word = self.bytes[self.pos..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'$')
                .count();
            if (c.is_ascii_alphabetic() || c == b'_' || c == b'$') && word > usize::from(c == b'$')
            {
                let text = &self.src[self.pos..self.pos + word];
                self.push(Tok::keyword(text).unwrap_or(Tok::Ident), word);
                continue;
            }
            if let Some((kind, len)) = self.punctuation() {
                self.push(kind, len);
                continue;
            }
            self.error();
        }
        self.tokens.push(Token {
            kind: Tok::Eof,
            start: self.bytes.len(),
            end: self.bytes.len(),
            line: self.line,
            column: self.column,
        });
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    /// Moves `len` bytes on, keeping the line and column.
    fn advance(&mut self, len: usize) {
        for ch in self.src[self.pos..self.pos + len].chars() {
            if ch == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
        self.pos += len;
    }

    fn push(&mut self, kind: Tok, len: usize) {
        self.tokens.push(Token {
            kind,
            start: self.pos,
            end: self.pos + len,
            line: self.line,
            column: self.column,
        });
        self.advance(len);
    }

    /// The length of a comment starting here, or 0.
    fn comment_len(&self) -> usize {
        let rest = &self.bytes[self.pos..];
        if rest.starts_with(b"//") {
            return rest.iter().take_while(|b| **b != b'\n').count();
        }
        if rest.starts_with(b"/*") {
            return rest[2..]
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(0, |end| end + 4);
        }
        0
    }

    /// The length of a path segment starting here, or 0.
    fn path_segment_len(&self) -> usize {
        let rest = &self.bytes[self.pos + 1..];
        if rest.first() == Some(&b'(') {
            let inner = segment_chars(&rest[1..]);
            if inner > 0 && rest.get(1 + inner) == Some(&b')') {
                return inner + 3;
            }
            return 0;
        }
        match segment_chars(rest) {
            0 => 0,
            n => n + 1,
        }
    }

    /// `1`, `1.5`, `1.` (a float) or `.5`.
    fn number_len(&self) -> usize {
        let rest = &self.bytes[self.pos..];
        let int = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if rest.get(int) == Some(&b'.') {
            let frac = rest[int + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            return int + 1 + frac;
        }
        int
    }

    fn punctuation(&self) -> Option<(Tok, usize)> {
        let two = match (self.bytes[self.pos], self.peek(1)) {
            (b'&', Some(b'&')) => Some(Tok::AndAnd),
            (b'|', Some(b'|')) => Some(Tok::OrOr),
            (b'=', Some(b'=')) => Some(Tok::EqEq),
            (b'!', Some(b'=')) => Some(Tok::Ne),
            (b'<', Some(b'=')) => Some(Tok::Le),
            (b'>', Some(b'=')) => Some(Tok::Ge),
            (b'-', Some(b'-')) => Some(Tok::Dec),
            (b'+', Some(b'+')) => Some(Tok::Inc),
            _ => None,
        };
        if let Some(kind) = two {
            return Some((kind, 2));
        }
        let one = match self.bytes[self.pos] {
            b'@' => Tok::At,
            b':' => Tok::Colon,
            b',' => Tok::Comma,
            b'$' => Tok::Dollar,
            b'.' => Tok::Dot,
            b'=' => Tok::Assign,
            b'>' => Tok::Gt,
            b'{' => Tok::LBrace,
            b'[' => Tok::LBracket,
            b'(' => Tok::LParen,
            b'<' => Tok::Lt,
            b'-' => Tok::Minus,
            b'%' => Tok::Percent,
            b'!' => Tok::Not,
            b'+' => Tok::Plus,
            b'?' => Tok::Question,
            b'}' => Tok::RBrace,
            b']' => Tok::RBracket,
            b')' => Tok::RParen,
            b';' => Tok::Semi,
            b'*' => Tok::Star,
            _ => return None,
        };
        Some((one, 1))
    }

    /// A character no token starts with. A lone `&` or `|` also takes the character after it
    /// (ANTLR consumed it looking for the second one), which is then lost.
    fn error(&mut self) {
        let start = self.pos;
        let first = self.src[start..].chars().next().expect("not at the end");
        let mut len = first.len_utf8();
        if matches!(first, '&' | '|')
            && let Some(next) = self.src[start + len..].chars().next()
        {
            len += next.len_utf8();
        }
        let text = self.src[start..start + len]
            .chars()
            .map(|c| match c {
                '\n' => "\\n".to_owned(),
                '\r' => "\\r".to_owned(),
                '\t' => "\\t".to_owned(),
                c => c.to_string(),
            })
            .collect();
        self.errors.push(LexError {
            start,
            line: self.line,
            column: self.column,
            text,
        });
        self.advance(len);
    }
}

/// The run of path segment characters at the start of `bytes`.
fn segment_chars(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || b"-._~&'*+:@".contains(&c) {
            i += 1;
        } else if c == b'%'
            && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
        {
            i += 3;
        } else {
            break;
        }
    }
    i
}

/// The length of the string starting with the quote at `bytes[0]`, and whether it is closed.
/// An unclosed string ends before the first character it cannot take.
fn string_len(bytes: &[u8]) -> (usize, bool) {
    let quote = bytes[0];
    let mut i = 1;
    loop {
        match bytes.get(i) {
            None | Some(b'\n' | b'\r' | 0x08 | 0x0c) => return (i, false),
            Some(&c) if c == quote => return (i + 1, true),
            Some(b'\\') => match escape_len(&bytes[i + 1..]) {
                Some(n) => i += 1 + n,
                None => return (i, false),
            },
            Some(_) => i += 1,
        }
    }
}

/// The length of the escape after a backslash, if it is one.
fn escape_len(bytes: &[u8]) -> Option<usize> {
    let hex = |n: usize| bytes.len() > n && bytes[1..=n].iter().all(u8::is_ascii_hexdigit);
    match bytes.first()? {
        b'n' | b'r' | b't' | b'b' | b'f' | b'\\' | b'\'' | b'"' => Some(1),
        b'x' if hex(2) => Some(3),
        b'u' if hex(4) => Some(5),
        b'0'..=b'3' if bytes.len() > 2 && bytes[1..3].iter().all(|b| (b'0'..=b'7').contains(b)) => {
            Some(3)
        }
        _ => None,
    }
}

/// The value of a closed string or bytes literal (`text` with its quotes and any `b` prefix).
pub(crate) fn unquote(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let bytes_literal = matches!(bytes[0], b'b' | b'B');
    let body = &bytes[usize::from(bytes_literal) + 1..bytes.len() - 1];
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'\\' {
            out.push(body[i]);
            i += 1;
            continue;
        }
        let e = body[i + 1];
        let simple = match e {
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'\\' | b'\'' | b'"' => Some(e),
            _ => None,
        };
        if let Some(v) = simple {
            out.push(v);
            i += 2;
            continue;
        }
        let (code, len) = match e {
            b'x' => (parse_radix(&body[i + 2..i + 4], 16), 4),
            b'u' => (parse_radix(&body[i + 2..i + 6], 16), 6),
            _ => (parse_radix(&body[i + 1..i + 4], 8), 4),
        };
        if bytes_literal || code < 0x80 {
            // A bytes literal takes the byte; `\xff` in a string is the code point.
            out.push(u8::try_from(code & 0xff).unwrap_or(0));
        } else {
            let ch = char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER);
            out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
        }
        i += len;
    }
    out
}

fn parse_radix(digits: &[u8], radix: u32) -> u32 {
    std::str::from_utf8(digits)
        .ok()
        .and_then(|s| u32::from_str_radix(s, radix).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Tok, &str)> {
        lex(src)
            .tokens
            .iter()
            .map(|t| (t.kind, &src[t.start..t.end]))
            .collect()
    }

    #[test]
    fn path_segments_and_division() {
        assert_eq!(
            kinds("/databases/$(db)/(default)"),
            vec![
                (Tok::PathSegment, "/databases"),
                (Tok::Slash, "/"),
                (Tok::Dollar, "$"),
                (Tok::LParen, "("),
                (Tok::Ident, "db"),
                (Tok::RParen, ")"),
                (Tok::PathSegment, "/(default)"),
                (Tok::Eof, ""),
            ]
        );
        // As on the official emulator, `4/2` is a number and a path segment.
        assert_eq!(kinds("4/2")[1], (Tok::PathSegment, "/2"));
        assert_eq!(kinds("4 / 2")[1], (Tok::Slash, "/"));
        assert_eq!(kinds("/a%20b:c@d")[0], (Tok::PathSegment, "/a%20b:c@d"));
        assert_eq!(kinds("/a%b")[0], (Tok::PathSegment, "/a"));
        assert_eq!(kinds("/a(b)")[0], (Tok::PathSegment, "/a"));
    }

    #[test]
    fn comments() {
        assert_eq!(
            kinds("a // b\nc"),
            vec![(Tok::Ident, "a"), (Tok::Ident, "c"), (Tok::Eof, "")]
        );
        assert_eq!(kinds("a /* b\n */ c").len(), 3);
        // An unclosed block comment is a path segment.
        assert_eq!(kinds("/*a")[0], (Tok::PathSegment, "/*a"));
        // `/a//b` is a segment and a comment.
        assert_eq!(
            kinds("/a//b {"),
            vec![(Tok::PathSegment, "/a"), (Tok::Eof, "")]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("1 1.5 .5 1. 1..2"),
            vec![
                (Tok::Int, "1"),
                (Tok::Float, "1.5"),
                (Tok::Float, ".5"),
                (Tok::Float, "1."),
                (Tok::Float, "1."),
                (Tok::Float, ".2"),
                (Tok::Eof, "")
            ]
        );
        assert_eq!(kinds("a.1")[1], (Tok::Float, ".1"));
        assert_eq!(kinds("1e3")[1], (Tok::Ident, "e3"));
        assert_eq!(
            kinds("$b a$ $ ("),
            vec![
                (Tok::Ident, "$b"),
                (Tok::Ident, "a$"),
                (Tok::Dollar, "$"),
                (Tok::LParen, "("),
                (Tok::Eof, "")
            ]
        );
    }

    #[test]
    fn strings() {
        assert_eq!(kinds(r#"'a\'b' "c""#)[0], (Tok::String, r"'a\'b'"));
        assert_eq!(kinds("b'ab'")[0], (Tok::Bytes, "b'ab'"));
        assert_eq!(kinds("'ab")[0], (Tok::UnpairedSingle, "'ab"));
        assert_eq!(kinds("\"a\nb\"")[0], (Tok::UnpairedDouble, "\"a"));
        assert_eq!(kinds(r"'\a'")[0], (Tok::UnpairedSingle, "'"));
        assert_eq!(unquote(r"'a\n\x41é\101'"), "a\nAéA".as_bytes());
        assert_eq!(unquote(r"b'\xff'"), vec![0xff]);
    }

    #[test]
    fn keywords_and_operators() {
        assert_eq!(
            kinds("--x ++ && || == != <= >= in is"),
            vec![
                (Tok::Dec, "--"),
                (Tok::Ident, "x"),
                (Tok::Inc, "++"),
                (Tok::AndAnd, "&&"),
                (Tok::OrOr, "||"),
                (Tok::EqEq, "=="),
                (Tok::Ne, "!="),
                (Tok::Le, "<="),
                (Tok::Ge, ">="),
                (Tok::In, "in"),
                (Tok::Is, "is"),
                (Tok::Eof, "")
            ]
        );
    }

    #[test]
    fn errors_and_positions() {
        let lexed = lex("a\u{a0}b\n\u{e9} &&& c");
        let errors: Vec<_> = lexed
            .errors
            .iter()
            .map(|e| (e.line, e.column, e.text.as_str()))
            .collect();
        assert_eq!(
            errors,
            vec![(1, 2, "\u{a0}"), (2, 1, "\u{e9}"), (2, 5, "& ")]
        );
        let c = lexed
            .tokens
            .iter()
            .find(|t| t.kind == Tok::Ident && t.line == 2);
        assert_eq!(c.map(|t| t.column), Some(7));
    }
}
