//! Request bodies as Gson reads them in lenient mode, which protobuf-java's `JsonFormat` turns on
//! (through `JsonParser.parseReader`) when the official emulator reads a REST request
//! (`tools/oracle/rest.py`, #262). [`strict`] rewrites such a body into the JSON serde_json
//! reads, or refuses it:
//!
//! - One byte order mark may come first, then the prefix `)]}'` and a newline after whitespace.
//! - Whitespace is a space, a tab, a carriage return or a newline; comments (`/* … */`, `//` or
//!   `#` to the end of the line) count as whitespace.
//! - Strings and names may be in double or single quotes, with raw control characters, the
//!   escapes `\"`, `\'`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t` and `\uXXXX`, and a backslash
//!   before a newline for the newline; or unquoted, up to a space, a tab, a line break, a form
//!   feed or one of `/\;#={}[]:,`.
//! - An unquoted `true`, `false` or `null`, in any case, is that literal; one that reads as a
//!   JSON number is that number, as written; any other is a string (`NaN`, `01`, `+1`).
//! - A name is followed by `:`, `=` or `=>`; pairs and elements are separated by `,` or `;`. An
//!   element left out (`[1,,2]`, `[1,]`) is a null, which no repeated field takes: refused.
//! - Whatever follows the first value is not read.

use serde_json::Value as Json;

/// The JSON text of `body` as Gson reads it, or `None` where Gson (or then `JsonFormat`) refuses
/// it. A body with no value at all is refused too: only an empty one is `{}`, and the caller
/// sees to that.
pub(crate) fn strict(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader {
        chars: text.chars().collect(),
        pos: 0,
        out: String::with_capacity(text.len()),
    };
    reader.non_execute_prefix()?;
    reader.value()?;
    Some(reader.out)
}

struct Reader {
    chars: Vec<char>,
    pos: usize,
    out: String,
}

/// Whether `c` may be part of an unquoted string or name.
fn literal(c: char) -> bool {
    !matches!(
        c,
        '/' | '\\'
            | ';'
            | '#'
            | '='
            | '{'
            | '}'
            | '['
            | ']'
            | ':'
            | ','
            | ' '
            | '\t'
            | '\u{c}'
            | '\r'
            | '\n'
    )
}

impl Reader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// The next character that is not whitespace or in a comment, consumed.
    fn next_token(&mut self) -> Option<char> {
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                ' ' | '\t' | '\r' | '\n' => {}
                '/' => match self.peek() {
                    Some('*') => {
                        self.pos += 1;
                        loop {
                            if self.peek()? == '*' && self.chars.get(self.pos + 1) == Some(&'/') {
                                self.pos += 2;
                                break;
                            }
                            self.pos += 1;
                        }
                    }
                    Some('/') => self.skip_line(),
                    _ => return Some(c),
                },
                '#' => self.skip_line(),
                _ => return Some(c),
            }
        }
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '\n' || c == '\r' {
                break;
            }
        }
    }

    /// `)]}'` and a newline after the leading whitespace, skipped; `None` when there is no
    /// value at all.
    fn non_execute_prefix(&mut self) -> Option<()> {
        self.next_token()?;
        self.pos -= 1;
        if self.chars[self.pos..].starts_with(&[')', ']', '}', '\'', '\n']) {
            self.pos += 5;
        }
        Some(())
    }

    fn value(&mut self) -> Option<()> {
        let c = self.next_token()?;
        match c {
            '{' => self.object(),
            '[' => self.array(),
            '"' | '\'' => self.quoted(c),
            c if literal(c) => {
                self.pos -= 1;
                self.unquoted_value();
                Some(())
            }
            _ => None,
        }
    }

    fn object(&mut self) -> Option<()> {
        self.out.push('{');
        let mut first = true;
        loop {
            let c = self.next_token()?;
            match c {
                '}' if first => break,
                '"' | '\'' => self.quoted(c)?,
                c if literal(c) => {
                    self.pos -= 1;
                    let name = self.unquoted();
                    self.out.push_str(&Json::String(name).to_string());
                }
                _ => return None,
            }
            match self.next_token()? {
                ':' => {}
                '=' => {
                    if self.peek() == Some('>') {
                        self.pos += 1;
                    }
                }
                _ => return None,
            }
            self.out.push(':');
            self.value()?;
            first = false;
            match self.next_token()? {
                ',' | ';' => self.out.push(','),
                '}' => break,
                _ => return None,
            }
        }
        self.out.push('}');
        Some(())
    }

    fn array(&mut self) -> Option<()> {
        self.out.push('[');
        if self.next_token()? == ']' {
            self.out.push(']');
            return Some(());
        }
        self.pos -= 1;
        loop {
            // An element left out is a null, which no repeated field takes.
            if matches!(self.next_token()?, ',' | ';' | ']') {
                return None;
            }
            self.pos -= 1;
            self.value()?;
            match self.next_token()? {
                ',' | ';' => self.out.push(','),
                ']' => break,
                _ => return None,
            }
        }
        self.out.push(']');
        Some(())
    }

    /// A string in `quote`s, its opening quote read.
    fn quoted(&mut self, quote: char) -> Option<()> {
        let mut text = String::new();
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                c if c == quote => break,
                '\\' => {
                    let escaped = self.peek()?;
                    self.pos += 1;
                    text.push(match escaped {
                        'u' => self.unicode_escape()?,
                        't' => '\t',
                        'b' => '\u{8}',
                        'n' | '\n' => '\n',
                        'r' => '\r',
                        'f' => '\u{c}',
                        '\'' | '"' | '\\' | '/' => escaped,
                        _ => return None,
                    });
                }
                c => text.push(c),
            }
        }
        self.out.push_str(&Json::String(text).to_string());
        Some(())
    }

    /// The character of a `\\u` escape, its `u` read: a surrogate pair is two escapes. A lone
    /// surrogate is refused, as hidane cannot store one (docs/parity-exceptions.md).
    fn unicode_escape(&mut self) -> Option<char> {
        let unit = |reader: &mut Self| {
            let hex: String = reader
                .chars
                .get(reader.pos..reader.pos + 4)?
                .iter()
                .collect();
            reader.pos += 4;
            u16::from_str_radix(&hex, 16).ok()
        };
        let high = unit(self)?;
        let mut units = vec![high];
        if (0xd800..0xdc00).contains(&high) {
            if self.chars.get(self.pos..self.pos + 2) != Some(&['\\', 'u']) {
                return None;
            }
            self.pos += 2;
            units.push(unit(self)?);
        }
        let mut decoded = char::decode_utf16(units);
        let c = decoded.next()?.ok()?;
        decoded.next().is_none().then_some(c)
    }

    fn unquoted(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(literal) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// An unquoted literal, number or string.
    fn unquoted_value(&mut self) {
        let text = self.unquoted();
        if ["true", "false", "null"]
            .iter()
            .any(|keyword| text.eq_ignore_ascii_case(keyword))
        {
            self.out.push_str(&text.to_ascii_lowercase());
        } else if number(&text) {
            self.out.push_str(&text);
        } else {
            self.out.push_str(&Json::String(text).to_string());
        }
    }
}

/// Whether `text` is a JSON number: `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
fn number(text: &str) -> bool {
    let digits = |s: &str| s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = text.strip_prefix('-').unwrap_or(text);
    let int = digits(rest);
    if int == 0 || (int > 1 && rest.starts_with('0')) {
        return false;
    }
    let mut rest = &rest[int..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let n = digits(fraction);
        if n == 0 {
            return false;
        }
        rest = &fraction[n..];
    }
    if let Some(exponent) = rest.strip_prefix(['e', 'E']) {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        let n = digits(exponent);
        if n == 0 {
            return false;
        }
        rest = &exponent[n..];
    }
    rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Option<String> {
        strict(text.as_bytes())
    }

    #[test]
    fn lenient_syntax_becomes_json() {
        for (text, json) in [
            ("{'writes': []}", r#"{"writes":[]}"#),
            ("\u{feff}{\"a\": 1}", r#"{"a":1}"#),
            (" )]}'\n{a: b}", r#"{"a":"b"}"#),
            (
                "/* c */ {a // c\n= 1; b => 'x\"y' # c\n}",
                r#"{"a":1,"b":"x\"y"}"#,
            ),
            (
                "{a: [TRUE; nUlL, NaN, 01, +1, 1.50, -0, 1e5, it's]}",
                r#"{"a":[true,null,"NaN","01","+1",1.50,-0,1e5,"it's"]}"#,
            ),
            (
                "{a: \"x\ty\\\nz\\u00e9\\ud83d\\ude00\"}",
                "{\"a\":\"x\\ty\\nzé😀\"}",
            ),
            ("{\"a\": 1} trailing } garbage \"", r#"{"a":1}"#),
            ("{}", "{}"),
            ("[]", "[]"),
        ] {
            assert_eq!(read(text).as_deref(), Some(json), "{text:?}");
        }
    }

    /// Refused here, or read as something other than a request (an object).
    #[test]
    fn what_gson_refuses() {
        for text in [
            "",
            "   ",
            "/* c */",
            "\u{feff}\u{feff}{}",
            ")]}'{}",
            "{a: 1,}",
            "{a: 1;}",
            "[1,]",
            "[,1]",
            "[1,,2]",
            "{\"a\": \"\\x41\"}",
            "{\"a\": \"\\u00zz\"}",
            "{\u{c}}",
            "{a/b: 1}",
            "{'a: 1}",
            "{a: 1 /* open",
            "{a 1}",
        ] {
            let object = read(text)
                .and_then(|json| serde_json::from_str::<Json>(&json).ok())
                .filter(Json::is_object);
            assert_eq!(object, None, "{text:?}");
        }
    }
}
