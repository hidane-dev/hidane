//! Diagnostics: the `issues[]` of a successful `PUT :securityRules` and the lines of its
//! "Error compiling rules:" message.

use crate::ast::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// Where an issue is, as the official emulator reports it: line and column are 1-based, the
/// offsets count code points from the start of the file, and `end_offset` is the last code
/// point of the span (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    pub line: u32,
    pub column: u32,
    pub current_offset: u32,
    pub end_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The file's name as the request gave it; `None` when it had none.
    pub file_name: Option<String>,
    /// `None` for issues about the whole ruleset (the old version warning), printed `L0:0`.
    pub position: Option<Position>,
    pub description: String,
    pub severity: Severity,
}

impl Issue {
    /// `L<line>:<column> <description>`, a line of the error message.
    pub fn line(&self) -> String {
        let (line, column) = self.position.map_or((0, 0), |p| (p.line, p.column));
        format!("L{line}:{column} {}", self.description)
    }
}

/// An issue found in one file, before it is given the file's name and code point offsets.
#[derive(Debug, Clone)]
pub(crate) struct Diag {
    pub span: Option<Span>,
    pub message: String,
    pub severity: Severity,
}

impl Diag {
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self {
            span: Some(span),
            message: message.into(),
            severity: Severity::Error,
        }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self {
            span: Some(span),
            message: message.into(),
            severity: Severity::Warning,
        }
    }

    pub fn into_issue(self, offsets: &Offsets, file_name: Option<&str>) -> Issue {
        Issue {
            file_name: file_name.map(str::to_owned),
            position: self.span.map(|span| Position {
                line: span.line,
                column: span.column,
                current_offset: offsets.at(span.start),
                // The last code point of the span; an empty span ends where it starts.
                end_offset: offsets.at(span.end).max(offsets.at(span.start) + 1) - 1,
            }),
            description: self.message,
            severity: self.severity,
        }
    }
}

/// How many code points precede each byte offset of a file (the byte offset itself in ASCII).
pub(crate) struct Offsets {
    len: usize,
    counts: Option<Vec<u32>>,
}

impl Offsets {
    pub fn new(src: &str) -> Self {
        let counts = (!src.is_ascii()).then(|| {
            let mut counts = Vec::with_capacity(src.len() + 1);
            for (count, c) in src.chars().enumerate() {
                let count = u32::try_from(count).unwrap_or(u32::MAX);
                counts.extend(std::iter::repeat_n(count, c.len_utf8()));
            }
            counts.push(u32::try_from(src.chars().count()).unwrap_or(u32::MAX));
            counts
        });
        Self {
            len: src.len(),
            counts,
        }
    }

    fn at(&self, offset: usize) -> u32 {
        let offset = offset.min(self.len);
        match &self.counts {
            Some(counts) => counts[offset],
            None => u32::try_from(offset).unwrap_or(u32::MAX),
        }
    }
}

/// The official order: by line, column and then text (`L0:0` first), duplicates kept.
pub(crate) fn sort(diags: &mut [Diag]) {
    diags.sort_by(|a, b| {
        let key = |d: &Diag| d.span.map_or((0, 0), |s| (s.line, s.column));
        key(a).cmp(&key(b)).then_with(|| a.message.cmp(&b.message))
    });
}
