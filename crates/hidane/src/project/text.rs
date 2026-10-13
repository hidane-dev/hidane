//! Text decoded from a file (a JSON string, a YAML scalar, a shell script), with the offset in
//! the file each of its bytes came from, so that an edit of the decoded text can be made to the
//! file itself.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub text: String,
    /// The file offset of each byte of `text`, and one more for the end.
    pub offsets: Vec<usize>,
}

impl Mapped {
    /// Text taken from the file as it is, starting at `start`.
    pub fn verbatim(text: &str, start: usize) -> Self {
        Self {
            text: text.to_owned(),
            offsets: (start..=start + text.len()).collect(),
        }
    }

    /// Builds a mapping byte by byte.
    pub fn builder() -> MappedBuilder {
        MappedBuilder {
            text: String::new(),
            offsets: Vec::new(),
        }
    }

    /// The file offset of byte `i` of the text (the end for `i == len`).
    pub fn offset(&self, i: usize) -> usize {
        self.offsets[i.min(self.offsets.len() - 1)]
    }
}

pub struct MappedBuilder {
    text: String,
    offsets: Vec<usize>,
}

impl MappedBuilder {
    /// Appends `c`, which came from the file at `offset`.
    pub fn push(&mut self, c: char, offset: usize) {
        for _ in 0..c.len_utf8() {
            self.offsets.push(offset);
        }
        self.text.push(c);
    }

    /// Appends `s`, which is in the file as it is from `offset`.
    pub fn push_str(&mut self, s: &str, offset: usize) {
        self.offsets.extend(offset..offset + s.len());
        self.text.push_str(s);
    }

    pub fn finish(mut self, end: usize) -> Mapped {
        self.offsets.push(end);
        Mapped {
            text: self.text,
            offsets: self.offsets,
        }
    }
}

/// The 1-based line of a byte offset in `src`.
pub fn line_of(src: &str, offset: usize) -> usize {
    src.as_bytes()[..offset.min(src.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// The line of `src` that holds `offset`, without its line break.
pub fn line_at(src: &str, offset: usize) -> &str {
    let offset = offset.min(src.len());
    let start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = src[offset..].find('\n').map_or(src.len(), |i| offset + i);
    src[start..end].trim_end_matches('\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let src = "a\nbc\r\nd";
        assert_eq!(line_of(src, 0), 1);
        assert_eq!(line_of(src, 3), 2);
        assert_eq!(line_of(src, 7), 3);
        assert_eq!(line_at(src, 3), "bc");
        let mapped = Mapped::verbatim("bc", 2);
        assert_eq!(mapped.offset(1), 3);
        assert_eq!(mapped.offset(2), 4);
    }
}
