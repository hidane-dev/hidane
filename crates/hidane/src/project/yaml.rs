//! The block YAML that GitHub Actions workflows are written in, read with the line and byte
//! position of every value, so that a step can be edited in place and the file's comments and
//! layout stay as they are.
//!
//! Block mappings and sequences, plain and quoted scalars, and literal (`|`) and folded (`>`)
//! block scalars are read. Flow collections (`[a, b]`, `{a: b}`), anchors, aliases, tags and
//! quoted scalars over several lines are kept as [`NodeKind::Opaque`]: the scan knows where they
//! are but not what they hold, and leaves them to the user. Lines it could not place are listed
//! in [`Doc::problems`].

use super::text::Mapped;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// Byte offsets of the line in the file, without its line break.
    pub start: usize,
    pub end: usize,
    /// Leading spaces.
    pub indent: usize,
    pub kind: LineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Blank,
    Comment,
    Content,
}

#[derive(Debug, Clone)]
pub struct Node {
    /// Line indexes (0-based) of the node's first and last lines.
    pub first: usize,
    pub last: usize,
    pub kind: NodeKind,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    Map(Vec<Entry>),
    Seq(Vec<Item>),
    Scalar(Scalar),
    Null,
    Opaque,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    /// The key's line index and column.
    pub line: usize,
    pub col: usize,
    pub value: Node,
}

#[derive(Debug, Clone)]
pub struct Item {
    /// The dash's line index and column.
    pub line: usize,
    pub col: usize,
    pub value: Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Single,
    Double,
    Literal,
    Folded,
}

#[derive(Debug, Clone)]
pub struct Scalar {
    pub style: Style,
    /// The value, mapped to the file.
    pub text: Mapped,
    /// Byte offsets of the scalar in the file: quotes included, a block scalar from its `|` or
    /// `>` to the end of its last line. A trailing comment is not part of it.
    pub start: usize,
    pub end: usize,
    /// Where a comment on the scalar's last line starts.
    pub comment: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub lines: Vec<Line>,
    pub root: Option<Node>,
    /// Line indexes the parser could not place.
    pub problems: Vec<usize>,
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entry(key).map(|e| &e.value)
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        match &self.kind {
            NodeKind::Map(entries) => entries.iter().rev().find(|e| e.key == key),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        match &self.kind {
            NodeKind::Map(entries) => entries,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[Item] {
        match &self.kind {
            NodeKind::Seq(items) => items,
            _ => &[],
        }
    }

    pub fn scalar(&self) -> Option<&Scalar> {
        match &self.kind {
            NodeKind::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// The value of a scalar, whatever its style.
    pub fn as_str(&self) -> Option<&str> {
        self.scalar().map(|s| s.text.text.as_str())
    }

    pub fn is_opaque(&self) -> bool {
        matches!(self.kind, NodeKind::Opaque)
    }
}

impl Entry {
    /// The entry's last line index.
    pub fn last(&self) -> usize {
        self.line.max(self.value.last)
    }
}

impl Item {
    pub fn last(&self) -> usize {
        self.line.max(self.value.last)
    }
}

pub fn parse(src: &str) -> Doc {
    let lines = split_lines(src);
    let mut parser = Parser {
        src,
        lines,
        problems: Vec::new(),
    };
    let mut first = 0;
    while let Some(n) = parser.next_content(first) {
        let text = parser.text(n, 0);
        if text == "---" || text.starts_with("--- ") || text.starts_with('%') {
            first = n + 1;
        } else {
            break;
        }
    }
    let root = parser.next_content(first).map(|n| {
        let indent = parser.lines[n].indent;
        parser.block(n, indent)
    });
    if let Some(root) = &root
        && let Some(extra) = parser.next_content(root.last + 1)
    {
        parser.problems.push(extra);
    }
    Doc {
        lines: parser.lines,
        root,
        problems: parser.problems,
    }
}

fn split_lines(src: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start <= src.len() {
        let end_nl = src[start..].find('\n').map_or(src.len(), |i| start + i);
        let end = if end_nl > start && src.as_bytes()[end_nl - 1] == b'\r' {
            end_nl - 1
        } else {
            end_nl
        };
        let text = &src[start..end];
        let indent = text.len() - text.trim_start_matches(' ').len();
        let rest = text[indent..].trim_start_matches([' ', '\t']);
        let kind = if rest.is_empty() {
            LineKind::Blank
        } else if rest.starts_with('#') {
            LineKind::Comment
        } else {
            LineKind::Content
        };
        lines.push(Line {
            start,
            end,
            indent,
            kind,
        });
        if end_nl == src.len() {
            break;
        }
        start = end_nl + 1;
    }
    lines
}

struct Parser<'a> {
    src: &'a str,
    lines: Vec<Line>,
    problems: Vec<usize>,
}

impl Parser<'_> {
    fn next_content(&self, from: usize) -> Option<usize> {
        (from..self.lines.len()).find(|&i| self.lines[i].kind == LineKind::Content)
    }

    /// The text of line `l` from column `col`.
    fn text(&self, l: usize, col: usize) -> &str {
        let line = self.lines[l];
        let from = (line.start + col).min(line.end);
        &self.src[from..line.end]
    }

    fn is_dash(&self, l: usize, col: usize) -> bool {
        let line = self.lines[l];
        if line.indent != col {
            return false;
        }
        let text = self.text(l, col);
        text == "-" || text.starts_with("- ") || text.starts_with("-\t")
    }

    /// The node whose content starts at line `l`, column `col`.
    fn block(&mut self, l: usize, col: usize) -> Node {
        let text = self.text(l, col);
        if text == "-" || text.starts_with("- ") || text.starts_with("-\t") {
            self.seq(l, col)
        } else if self.key_at(l, col).is_some() {
            self.map(l, col)
        } else {
            let parent = col.saturating_sub(1);
            self.value(l, col, parent)
        }
    }

    /// A mapping key at line `l`, column `col`: the key and the column after its colon.
    fn key_at(&self, l: usize, col: usize) -> Option<(String, usize)> {
        let text = self.text(l, col);
        let after_colon = |rest: &str, used: usize| -> Option<usize> {
            let rest_trim = rest.trim_start_matches([' ', '\t']);
            let skipped = rest.len() - rest_trim.len();
            let after = rest_trim.strip_prefix(':')?;
            if after.is_empty() || after.starts_with([' ', '\t']) {
                Some(col + used + skipped + 1)
            } else {
                None
            }
        };
        match text.chars().next()? {
            q @ ('"' | '\'') => {
                let close = text[1..].find(q)? + 1;
                let key = text[1..close].to_owned();
                let at = after_colon(&text[close + 1..], close + 1)?;
                Some((key, at))
            }
            '-' if text.len() == 1 || text[1..].starts_with([' ', '\t']) => None,
            '#' | '[' | '{' | '&' | '*' | '!' | '|' | '>' | '%' | '@' | '`' | '?' => None,
            _ => {
                let bytes = text.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    match bytes[i] {
                        b':' if i + 1 == bytes.len() || matches!(bytes[i + 1], b' ' | b'\t') => {
                            let key = text[..i].trim_end();
                            return (!key.is_empty()).then(|| (key.to_owned(), col + i + 1));
                        }
                        b'#' if i > 0 && matches!(bytes[i - 1], b' ' | b'\t') => return None,
                        _ => i += 1,
                    }
                }
                None
            }
        }
    }

    fn map(&mut self, l: usize, col: usize) -> Node {
        let mut entries = Vec::new();
        let mut line = l;
        let mut last = l;
        loop {
            let Some((key, after)) = self.key_at(line, col) else {
                self.problems.push(line);
                break;
            };
            let value = self.after_key(line, after, col);
            last = last.max(line).max(value.last);
            entries.push(Entry {
                key,
                line,
                col,
                value,
            });
            match self.next_content(last + 1) {
                Some(n) if self.lines[n].indent == col && self.key_at(n, col).is_some() => {
                    line = n;
                }
                Some(n) if self.lines[n].indent > col => {
                    // More indented than the keys but part of no value: not understood.
                    self.problems.push(n);
                    last = self.skip_deeper(n, col);
                    match self.next_content(last + 1) {
                        Some(m) if self.lines[m].indent == col && self.key_at(m, col).is_some() => {
                            line = m;
                        }
                        _ => break,
                    }
                }
                _ => break,
            }
        }
        Node {
            first: l,
            last,
            kind: NodeKind::Map(entries),
        }
    }

    /// The last line of the lines from `n` on that are indented more than `col`.
    fn skip_deeper(&self, n: usize, col: usize) -> usize {
        let mut last = n;
        let mut i = n + 1;
        while i < self.lines.len() {
            let line = self.lines[i];
            if line.kind != LineKind::Blank && line.indent <= col {
                break;
            }
            if line.kind != LineKind::Blank {
                last = i;
            }
            i += 1;
        }
        last
    }

    /// The value of the key on line `l` whose colon ends before column `at`.
    fn after_key(&mut self, l: usize, at: usize, key_col: usize) -> Node {
        let text = self.text(l, at);
        let rest = text.trim_start_matches([' ', '\t']);
        if rest.is_empty() || rest.starts_with('#') {
            return match self.next_content(l + 1) {
                Some(n)
                    if self.lines[n].indent > key_col
                        || self.lines[n].indent == key_col && self.is_dash(n, key_col) =>
                {
                    let indent = self.lines[n].indent;
                    self.block(n, indent)
                }
                _ => Node {
                    first: l,
                    last: l,
                    kind: NodeKind::Null,
                },
            };
        }
        let col = at + (text.len() - rest.len());
        self.value(l, col, key_col)
    }

    fn seq(&mut self, l: usize, col: usize) -> Node {
        let mut items = Vec::new();
        let mut line = l;
        let mut last = l;
        loop {
            let after_dash = self.text(line, col + 1);
            let rest = after_dash.trim_start_matches([' ', '\t']);
            let item_col = col + 1 + (after_dash.len() - rest.len());
            let value = if rest.is_empty() || rest.starts_with('#') {
                match self.next_content(line + 1) {
                    Some(n) if self.lines[n].indent > col => {
                        let indent = self.lines[n].indent;
                        self.block(n, indent)
                    }
                    _ => Node {
                        first: line,
                        last: line,
                        kind: NodeKind::Null,
                    },
                }
            } else if rest == "-" || rest.starts_with("- ") {
                self.seq_inline(line, item_col)
            } else if self.key_at(line, item_col).is_some() {
                self.map(line, item_col)
            } else {
                self.value(line, item_col, col)
            };
            last = last.max(line).max(value.last);
            items.push(Item { line, col, value });
            match self.next_content(last + 1) {
                Some(n) if self.is_dash(n, col) => line = n,
                _ => break,
            }
        }
        Node {
            first: l,
            last,
            kind: NodeKind::Seq(items),
        }
    }

    /// A sequence that starts in the middle of a line (`- - a`); its later items are on lines
    /// indented to `col`.
    fn seq_inline(&mut self, l: usize, col: usize) -> Node {
        // Treat it as opaque: nested sequences do not occur in the parts of a workflow the
        // scan reads.
        let last = self.skip_deeper(l, col.saturating_sub(1));
        Node {
            first: l,
            last,
            kind: NodeKind::Opaque,
        }
    }

    /// A value starting at line `l`, column `col`, in a collection indented to `parent`.
    fn value(&mut self, l: usize, col: usize, parent: usize) -> Node {
        let text = self.text(l, col);
        let start = self.lines[l].start + col;
        match text.as_bytes().first() {
            Some(b'|' | b'>') => self.block_scalar(l, col, parent),
            Some(b'"') => self.quoted(l, col, parent, Style::Double),
            Some(b'\'') => self.quoted(l, col, parent, Style::Single),
            Some(b'[' | b'{') => {
                let last = match flow_end(text) {
                    Some(_) => l,
                    None => self.skip_deeper(l, parent),
                };
                Node {
                    first: l,
                    last,
                    kind: NodeKind::Opaque,
                }
            }
            Some(b'&' | b'!') => {
                let last = self.skip_deeper(l, parent);
                Node {
                    first: l,
                    last,
                    kind: NodeKind::Opaque,
                }
            }
            Some(b'*') => Node {
                first: l,
                last: l,
                kind: NodeKind::Opaque,
            },
            Some(b'%' | b'@' | b'`') => {
                self.problems.push(l);
                Node {
                    first: l,
                    last: l,
                    kind: NodeKind::Opaque,
                }
            }
            _ => self.plain(l, col, parent, start),
        }
    }

    fn plain(&mut self, l: usize, col: usize, parent: usize, start: usize) -> Node {
        let text = self.text(l, col);
        let (value, comment) = strip_comment(text);
        let value_trim = value.trim_end_matches([' ', '\t']);
        let mut last = l;
        let mut continued = Vec::new();
        let mut i = l + 1;
        while i < self.lines.len() {
            let line = self.lines[i];
            match line.kind {
                LineKind::Blank => {}
                // A key more indented than the value's own: not a continuation, and not
                // valid YAML either (the mapping reports it).
                LineKind::Content
                    if line.indent > parent && self.key_at(i, line.indent).is_some() =>
                {
                    break;
                }
                LineKind::Content if line.indent > parent => {
                    continued.push(i);
                    last = i;
                }
                _ => break,
            }
            i += 1;
        }
        let comment_at = comment.map(|c| start + c);
        if continued.is_empty() {
            return Node {
                first: l,
                last: l,
                kind: NodeKind::Scalar(Scalar {
                    style: Style::Plain,
                    text: Mapped::verbatim(value_trim, start),
                    start,
                    end: start + value_trim.len(),
                    comment: comment_at,
                }),
            };
        }
        // Lines folded into one, separated by spaces (a blank line is a line break).
        let mut out = Mapped::builder();
        out.push_str(value_trim, start);
        let mut prev = l;
        let mut end = start + value_trim.len();
        let mut comment = None;
        for &n in &continued {
            let line = self.lines[n];
            let raw = &self.src[line.start + line.indent..line.end];
            let (part, part_comment) = strip_comment(raw);
            let part = part.trim_end_matches([' ', '\t']);
            let sep = if n > prev + 1 { '\n' } else { ' ' };
            out.push(sep, self.lines[prev].end);
            out.push_str(part, line.start + line.indent);
            end = line.start + line.indent + part.len();
            comment = part_comment.map(|c| line.start + line.indent + c);
            prev = n;
        }
        Node {
            first: l,
            last,
            kind: NodeKind::Scalar(Scalar {
                style: Style::Plain,
                text: out.finish(end),
                start,
                end,
                comment,
            }),
        }
    }

    fn quoted(&mut self, l: usize, col: usize, parent: usize, style: Style) -> Node {
        let line = self.lines[l];
        let start = line.start + col;
        let text = &self.src[start..line.end];
        let mut out = Mapped::builder();
        let bytes = text.as_bytes();
        let mut i = 1;
        let close = loop {
            if i >= bytes.len() {
                break None;
            }
            let c = text[i..].chars().next().unwrap_or(' ');
            match (style, c) {
                (Style::Single, '\'') if bytes.get(i + 1) == Some(&b'\'') => {
                    out.push('\'', start + i);
                    i += 2;
                }
                (Style::Single, '\'') | (Style::Double, '"') => break Some(i),
                (Style::Double, '\\') => {
                    let Some(&e) = bytes.get(i + 1) else {
                        break None;
                    };
                    let (decoded, used) = match e {
                        b'n' => ('\n', 2),
                        b't' => ('\t', 2),
                        b'r' => ('\r', 2),
                        b'0' => ('\0', 2),
                        b'"' => ('"', 2),
                        b'\\' => ('\\', 2),
                        b'/' => ('/', 2),
                        b' ' => (' ', 2),
                        b'x' | b'u' | b'U' => {
                            let width = match e {
                                b'x' => 2,
                                b'u' => 4,
                                _ => 8,
                            };
                            let hex = text.get(i + 2..i + 2 + width).unwrap_or("");
                            let c = u32::from_str_radix(hex, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .unwrap_or('\u{fffd}');
                            (c, 2 + width)
                        }
                        _ => ('\u{fffd}', 2),
                    };
                    out.push(decoded, start + i);
                    i += used;
                }
                (_, c) => {
                    out.push(c, start + i);
                    i += c.len_utf8();
                }
            }
        };
        match close {
            Some(close) => {
                let after = &text[close + 1..];
                let (rest, comment) = strip_comment(after);
                if !rest.trim().is_empty() {
                    self.problems.push(l);
                }
                Node {
                    first: l,
                    last: l,
                    kind: NodeKind::Scalar(Scalar {
                        style,
                        text: out.finish(start + close),
                        start,
                        end: start + close + 1,
                        comment: comment.map(|c| start + close + 1 + c),
                    }),
                }
            }
            // Over several lines: not read.
            None => {
                let last = self.skip_deeper(l, parent);
                Node {
                    first: l,
                    last,
                    kind: NodeKind::Opaque,
                }
            }
        }
    }

    fn block_scalar(&mut self, l: usize, col: usize, parent: usize) -> Node {
        let header = self.text(l, col);
        let style = if header.starts_with('|') {
            Style::Literal
        } else {
            Style::Folded
        };
        let (indicators, comment) = strip_comment(&header[1..]);
        let indicators = indicators.trim_end();
        let mut explicit = None;
        for c in indicators.chars() {
            match c {
                '-' | '+' => {}
                '1'..='9' => explicit = c.to_digit(10).map(|d| d as usize),
                _ => {
                    self.problems.push(l);
                    return Node {
                        first: l,
                        last: self.skip_deeper(l, parent),
                        kind: NodeKind::Opaque,
                    };
                }
            }
        }
        let mut body = Vec::new();
        let mut i = l + 1;
        while i < self.lines.len() {
            let line = self.lines[i];
            if line.kind != LineKind::Blank && line.indent <= parent {
                break;
            }
            body.push(i);
            i += 1;
        }
        while body
            .last()
            .is_some_and(|&n| self.lines[n].kind == LineKind::Blank)
        {
            body.pop();
        }
        let header_start = self.lines[l].start + col;
        let header_comment = comment.map(|c| header_start + 1 + c);
        let Some(&last) = body.last() else {
            return Node {
                first: l,
                last: l,
                kind: NodeKind::Scalar(Scalar {
                    style,
                    text: Mapped::verbatim("", self.lines[l].end),
                    start: header_start,
                    end: self.lines[l].end,
                    comment: header_comment,
                }),
            };
        };
        let content_indent = match explicit {
            Some(n) => parent + n,
            None => body
                .iter()
                .map(|&n| self.lines[n])
                .find(|line| line.kind != LineKind::Blank)
                .map_or(parent + 1, |line| line.indent),
        };
        let mut out = Mapped::builder();
        let mut prev_blank = false;
        for (k, &n) in body.iter().enumerate() {
            let line = self.lines[n];
            let blank = line.start + content_indent >= line.end
                && self.src[line.start..line.end].trim().is_empty();
            if !blank && line.indent < content_indent {
                // Less indented than the first line: not valid YAML.
                self.problems.push(n);
                return Node {
                    first: l,
                    last,
                    kind: NodeKind::Opaque,
                };
            }
            if k > 0 {
                let prev_end = self.lines[body[k - 1]].end;
                let sep = if style == Style::Literal || blank || prev_blank {
                    '\n'
                } else {
                    ' '
                };
                out.push(sep, prev_end);
            }
            if !blank {
                let from = line.start + content_indent;
                out.push_str(&self.src[from..line.end], from);
            }
            prev_blank = blank;
        }
        let end = self.lines[last].end;
        out.push('\n', end);
        Node {
            first: l,
            last,
            kind: NodeKind::Scalar(Scalar {
                style,
                text: out.finish(end),
                start: header_start,
                end,
                comment: None,
            }),
        }
    }
}

/// Splits a comment (` #…`) off a plain value: the value and where the comment starts.
fn strip_comment(text: &str) -> (&str, Option<usize>) {
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'#' && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t')) {
            return (&text[..i], Some(i));
        }
    }
    (text, None)
}

/// Where a flow collection that starts `text` closes on the same line.
fn flow_end(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            (None, '#') if i > 0 && text[..i].ends_with(' ') => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKFLOW: &str = "\
name: CI # the name
on:
  push:
    branches: [main]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # Java for the emulators.
      - name: Set up Java
        uses: actions/setup-java@v4 # v4.7.1
        with:
          distribution: temurin
          java-version: '21'
      - run: |
          npm ci
          # a comment in the script
          npx firebase emulators:exec --only firestore \"npm test\"

      - run: >
          echo one
          two
steps:
- uses: x
  with:
    a: b
- run: 'it''s'
";

    #[test]
    fn a_workflow() {
        let doc = parse(WORKFLOW);
        assert!(doc.problems.is_empty(), "{:?}", doc.problems);
        let root = doc.root.as_ref().unwrap();
        assert_eq!(root.get("name").unwrap().as_str(), Some("CI"));
        let steps = root
            .get("jobs")
            .unwrap()
            .get("test")
            .unwrap()
            .get("steps")
            .unwrap();
        let items = steps.items();
        assert_eq!(items.len(), 4);
        assert_eq!(
            items[0].value.get("uses").unwrap().as_str(),
            Some("actions/checkout@v4")
        );
        let java = &items[1];
        assert_eq!(java.line, 10);
        let uses = java.value.get("uses").unwrap().scalar().unwrap();
        assert_eq!(uses.text.text, "actions/setup-java@v4");
        assert_eq!(&WORKFLOW[uses.start..uses.end], "actions/setup-java@v4");
        assert_eq!(
            &WORKFLOW[uses.comment.unwrap()..uses.comment.unwrap() + 4],
            "# v4"
        );
        let with = java.value.entry("with").unwrap();
        assert_eq!((with.line, with.last()), (12, 14));
        assert_eq!(with.value.get("java-version").unwrap().as_str(), Some("21"));
        let run = items[2].value.get("run").unwrap().scalar().unwrap();
        assert_eq!(run.style, Style::Literal);
        assert_eq!(
            run.text.text,
            "npm ci\n# a comment in the script\nnpx firebase emulators:exec --only firestore \"npm test\"\n"
        );
        let at = run.text.text.find("npx").unwrap();
        assert_eq!(
            &WORKFLOW[run.text.offset(at)..run.text.offset(at) + 3],
            "npx"
        );
        assert_eq!(items[2].last(), 18);
        let folded = items[3].value.get("run").unwrap().as_str().unwrap();
        assert_eq!(folded, "echo one two\n");
        let top = root.get("steps").unwrap().items();
        assert_eq!(top.len(), 2);
        assert_eq!(
            top[0].value.get("with").unwrap().get("a").unwrap().as_str(),
            Some("b")
        );
        assert_eq!(top[1].value.get("run").unwrap().as_str(), Some("it's"));
    }

    #[test]
    fn what_is_not_read() {
        let doc = parse("a: [1, 2]\nb: {c: d}\nc: &x\n  d: e\nd: *x\ne: \"multi\n  line\"\nf: g\n");
        let root = doc.root.as_ref().unwrap();
        for key in ["a", "b", "c", "d", "e"] {
            assert!(root.get(key).unwrap().is_opaque(), "{key}");
        }
        assert_eq!(root.get("f").unwrap().as_str(), Some("g"));
        let doc = parse("a: b\n   c: d\n");
        assert_eq!(doc.problems, [1]);
    }

    #[test]
    fn double_quotes_map_their_escapes() {
        let src = "run: \"a \\\"b\\\" c\"\n";
        let doc = parse(src);
        let run = doc
            .root
            .as_ref()
            .unwrap()
            .get("run")
            .unwrap()
            .scalar()
            .unwrap();
        assert_eq!(run.text.text, "a \"b\" c");
        assert_eq!(&src[run.text.offset(2)..run.text.offset(2) + 2], "\\\"");
        assert_eq!(&src[run.start..run.end], "\"a \\\"b\\\" c\"");
    }

    #[test]
    fn crlf_and_document_start() {
        let src = "---\r\nkey: value\r\nlist:\r\n- a\r\n";
        let doc = parse(src);
        let root = doc.root.as_ref().unwrap();
        assert_eq!(root.get("key").unwrap().as_str(), Some("value"));
        assert_eq!(root.get("list").unwrap().items().len(), 1);
    }
}
