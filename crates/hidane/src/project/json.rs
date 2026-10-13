//! JSON with the position of every value, so that a string can be edited in place: a
//! `package.json` script gains a prefix without the rest of the file being re-serialised.
//! `firebase.json` may hold comments (firebase-tools reads it with `cjson`), which
//! [`parse_with_comments`] skips.

use super::text::Mapped;

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Byte offsets of the value in the file.
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Null,
    Bool(bool),
    Number(f64),
    Str(Mapped),
    Array(Vec<Node>),
    Object(Vec<(Mapped, Node)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

pub fn parse(src: &str) -> Result<Node, Error> {
    Parser::new(src, false).document()
}

/// JSON with `//` and `/* */` comments.
pub fn parse_with_comments(src: &str) -> Result<Node, Error> {
    Parser::new(src, true).document()
}

impl Node {
    /// The value of `key` (the last one, as `JSON.parse` keeps it).
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.kind {
            Kind::Object(entries) => entries
                .iter()
                .rev()
                .find(|(k, _)| k.text == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at a dotted path of keys.
    pub fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |node, key| node.get(key))
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::Str(s) => Some(&s.text),
            _ => None,
        }
    }

    pub fn as_mapped(&self) -> Option<&Mapped> {
        match &self.kind {
            Kind::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.kind {
            Kind::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self.kind {
            // A port: a whole number in range.
            Kind::Number(n) if n >= 0.0 && n.fract() == 0.0 && n <= u64::MAX as f64 => {
                Some(n as u64)
            }
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Node]> {
        match &self.kind {
            Kind::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[(Mapped, Node)] {
        match &self.kind {
            Kind::Object(entries) => entries,
            _ => &[],
        }
    }

    pub fn is_object(&self) -> bool {
        matches!(self.kind, Kind::Object(_))
    }
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    comments: bool,
    depth: usize,
}

const MAX_DEPTH: usize = 256;

impl<'a> Parser<'a> {
    fn new(src: &'a str, comments: bool) -> Self {
        let pos = if src.starts_with('\u{feff}') { 3 } else { 0 };
        Self {
            src,
            bytes: src.as_bytes(),
            pos,
            comments,
            depth: 0,
        }
    }

    fn error<T>(&self, message: &str) -> Result<T, Error> {
        Err(Error {
            offset: self.pos,
            message: message.to_owned(),
        })
    }

    fn document(mut self) -> Result<Node, Error> {
        self.space()?;
        let node = self.value()?;
        self.space()?;
        if self.pos < self.bytes.len() {
            return self.error("unexpected text after the value");
        }
        Ok(node)
    }

    fn space(&mut self) -> Result<(), Error> {
        loop {
            match self.bytes.get(self.pos) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'/') if self.comments => match self.bytes.get(self.pos + 1) {
                    Some(b'/') => {
                        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                            self.pos += 1;
                        }
                    }
                    Some(b'*') => match self.src[self.pos + 2..].find("*/") {
                        Some(end) => self.pos += 2 + end + 2,
                        None => return self.error("unterminated comment"),
                    },
                    _ => return self.error("unexpected '/'"),
                },
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self) -> Result<Node, Error> {
        let start = self.pos;
        let kind = match self.bytes.get(self.pos) {
            Some(b'{') => self.object()?,
            Some(b'[') => self.array()?,
            Some(b'"') => Kind::Str(self.string()?),
            Some(b't') => self.literal("true", Kind::Bool(true))?,
            Some(b'f') => self.literal("false", Kind::Bool(false))?,
            Some(b'n') => self.literal("null", Kind::Null)?,
            Some(b'-' | b'0'..=b'9') => self.number()?,
            Some(_) => return self.error("unexpected character"),
            None => return self.error("unexpected end"),
        };
        Ok(Node {
            start,
            end: self.pos,
            kind,
        })
    }

    fn literal(&mut self, word: &str, kind: Kind) -> Result<Kind, Error> {
        if self.src[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(kind)
        } else {
            self.error("unexpected word")
        }
    }

    fn number(&mut self) -> Result<Kind, Error> {
        let start = self.pos;
        while let Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') = self.bytes.get(self.pos) {
            self.pos += 1;
        }
        match self.src[start..self.pos].parse() {
            Ok(n) => Ok(Kind::Number(n)),
            Err(_) => self.error("invalid number"),
        }
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.error("nested too deeply");
        }
        self.pos += 1;
        self.space()
    }

    fn object(&mut self) -> Result<Kind, Error> {
        self.enter()?;
        let mut entries = Vec::new();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Kind::Object(entries));
        }
        loop {
            if self.bytes.get(self.pos) != Some(&b'"') {
                return self.error("expected a key");
            }
            let key = self.string()?;
            self.space()?;
            if self.bytes.get(self.pos) != Some(&b':') {
                return self.error("expected ':'");
            }
            self.pos += 1;
            self.space()?;
            let value = self.value()?;
            entries.push((key, value));
            self.space()?;
            match self.bytes.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                    self.space()?;
                }
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Kind::Object(entries));
                }
                _ => return self.error("expected ',' or '}'"),
            }
        }
    }

    fn array(&mut self) -> Result<Kind, Error> {
        self.enter()?;
        let mut items = Vec::new();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Kind::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.space()?;
            match self.bytes.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                    self.space()?;
                }
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Kind::Array(items));
                }
                _ => return self.error("expected ',' or ']'"),
            }
        }
    }

    fn string(&mut self) -> Result<Mapped, Error> {
        self.pos += 1;
        let mut out = Mapped::builder();
        loop {
            let at = self.pos;
            let Some(c) = self.src[self.pos..].chars().next() else {
                return self.error("unterminated string");
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out.finish(at));
                }
                '\\' => {
                    let escaped = match self.bytes.get(self.pos + 1) {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            let c = self.unicode_escape()?;
                            out.push(c, at);
                            continue;
                        }
                        _ => return self.error("invalid escape"),
                    };
                    self.pos += 2;
                    out.push(escaped, at);
                }
                c if (c as u32) < 0x20 => return self.error("control character in string"),
                c => {
                    self.pos += c.len_utf8();
                    out.push(c, at);
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, Error> {
        let unit = |p: &Self, at: usize| -> Option<u32> {
            let hex = p.src.get(at + 2..at + 6)?;
            if p.src.get(at..at + 2) != Some("\\u") {
                return None;
            }
            u32::from_str_radix(hex, 16).ok()
        };
        let Some(first) = unit(self, self.pos) else {
            return self.error("invalid unicode escape");
        };
        self.pos += 6;
        if (0xd800..0xdc00).contains(&first)
            && let Some(second) = unit(self, self.pos)
            && (0xdc00..0xe000).contains(&second)
        {
            self.pos += 6;
            let c = 0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
            return Ok(char::from_u32(c).unwrap_or('\u{fffd}'));
        }
        Ok(char::from_u32(first).unwrap_or('\u{fffd}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_positions() {
        let src = r#"{"scripts": {"test": "firebase emulators:exec \"npm t\""}, "n": 8080}"#;
        let doc = parse(src).unwrap();
        let script = doc.at(&["scripts", "test"]).unwrap();
        let text = script.as_mapped().unwrap();
        assert_eq!(text.text, r#"firebase emulators:exec "npm t""#);
        assert_eq!(&src[text.offset(0)..text.offset(8)], "firebase");
        // The decoded quote maps to its escape.
        let quote = text.text.find('"').unwrap();
        assert_eq!(&src[text.offset(quote)..text.offset(quote) + 2], "\\\"");
        assert_eq!(text.offset(text.text.len()), script.end - 1);
        assert_eq!(doc.get("n").unwrap().as_u64(), Some(8080));
    }

    #[test]
    fn comments_only_when_asked() {
        let src = "{\n  // the emulators\n  \"emulators\": {} /* none */\n}";
        assert!(parse(src).is_err());
        assert!(parse_with_comments(src).unwrap().get("emulators").is_some());
    }

    #[test]
    fn errors_and_escapes() {
        assert!(parse("{").is_err());
        assert!(parse("{\"a\": 1,}").is_err());
        assert!(parse("[1] x").is_err());
        let doc = parse(r#""é😀""#).unwrap();
        assert_eq!(doc.as_str(), Some("é😀"));
        let deep = "[".repeat(1000);
        assert!(parse(&deep).is_err());
    }
}
