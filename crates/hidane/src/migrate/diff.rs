//! Edits of a file's text, and the unified diff they make: computed from the edits themselves,
//! so a diff shows exactly the lines an edit touches, with three lines of context, in the
//! format `git apply` and `patch -p1` read.

use std::fmt::Write as _;

/// Replaces the bytes `start..end` of a text with `text` (an insertion when they are equal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// The text with the edits made. The edits must not overlap.
pub fn apply(src: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.start, e.end));
    let mut out = String::with_capacity(src.len() + 64);
    let mut at = 0;
    for edit in sorted {
        out.push_str(&src[at..edit.start]);
        out.push_str(&edit.text);
        at = edit.end;
    }
    out.push_str(&src[at..]);
    out
}

/// Whether two edits touch the same bytes (two insertions at one place count).
pub fn overlap(a: &TextEdit, b: &TextEdit) -> bool {
    a.start < b.end && b.start < a.end || a.start == b.start
}

const CONTEXT: usize = 3;

/// The lines of `src` (each with its line break, if it has one).
fn lines(src: &str) -> Vec<&str> {
    src.split_inclusive('\n').collect()
}

/// The unified diff of the edits to the file at `path` (empty without edits).
pub fn unified(path: &str, src: &str, edits: &[TextEdit]) -> String {
    if edits.is_empty() {
        return String::new();
    }
    let old = lines(src);
    let starts: Vec<usize> = std::iter::once(0)
        .chain(old.iter().scan(0, |at, l| {
            *at += l.len();
            Some(*at)
        }))
        .collect();
    // The line holding byte `offset`.
    let line_of = |offset: usize| -> usize {
        match starts.binary_search(&offset) {
            Ok(i) => i.min(old.len()),
            Err(i) => i - 1,
        }
    };
    // Each edit as a range of whole old lines: (first, end), end exclusive.
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.start, e.end));
    let mut groups: Vec<(usize, usize, Vec<&TextEdit>)> = Vec::new();
    for edit in sorted {
        // At the end of a last line without a line break, the edit changes that line.
        let first = if edit.start == src.len() && !src.is_empty() && !src.ends_with('\n') {
            old.len() - 1
        } else {
            line_of(edit.start)
        };
        // An insertion of whole lines before a line touches no old line.
        let whole_lines = starts.get(first) == Some(&edit.start) && edit.text.ends_with('\n');
        let end = if edit.end > edit.start {
            line_of(edit.end - 1) + 1
        } else if whole_lines || first == old.len() {
            first
        } else {
            first + 1
        };
        match groups.last_mut() {
            Some((_, last_end, members)) if first <= *last_end => {
                *last_end = (*last_end).max(end);
                members.push(edit);
            }
            _ => groups.push((first, end, vec![edit])),
        }
    }
    // The new lines of each group.
    let changed: Vec<(usize, usize, Vec<String>)> = groups
        .into_iter()
        .map(|(first, end, members)| {
            let from = starts[first];
            let to = starts[end.min(old.len())];
            let shifted: Vec<TextEdit> = members
                .iter()
                .map(|e| TextEdit {
                    start: e.start - from,
                    end: e.end - from,
                    text: e.text.clone(),
                })
                .collect();
            let new_text = apply(&src[from..to], &shifted);
            let new_lines = new_text.split_inclusive('\n').map(str::to_owned).collect();
            (first, end, new_lines)
        })
        .collect();
    // Hunks: groups closer than twice the context share one.
    let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    let mut delta: isize = 0;
    let mut i = 0;
    while i < changed.len() {
        let mut j = i;
        while j + 1 < changed.len() && changed[j + 1].0 <= changed[j].1 + 2 * CONTEXT {
            j += 1;
        }
        let start = changed[i].0.saturating_sub(CONTEXT);
        let end = (changed[j].1 + CONTEXT).min(old.len());
        let mut body = String::new();
        let mut old_count = 0;
        let mut new_count = 0;
        let mut line = start;
        for (first, last, new_lines) in &changed[i..=j] {
            for l in &old[line..*first] {
                push_line(&mut body, ' ', l);
                old_count += 1;
                new_count += 1;
            }
            for l in &old[*first..*last] {
                push_line(&mut body, '-', l);
                old_count += 1;
            }
            for l in new_lines {
                push_line(&mut body, '+', l);
                new_count += 1;
            }
            line = *last;
        }
        for l in &old[line..end] {
            push_line(&mut body, ' ', l);
            old_count += 1;
            new_count += 1;
        }
        let old_start = if old_count == 0 { start } else { start + 1 };
        let new_first = (start as isize + delta) as usize;
        let new_start = if new_count == 0 {
            new_first
        } else {
            new_first + 1
        };
        let _ = writeln!(
            out,
            "@@ -{} +{} @@",
            range(old_start, old_count),
            range(new_start, new_count)
        );
        out.push_str(&body);
        delta += new_count as isize - old_count as isize;
        i = j + 1;
    }
    out
}

fn range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

fn push_line(out: &mut String, mark: char, line: &str) {
    out.push(mark);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push_str("\n\\ No newline at end of file\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, end: usize, text: &str) -> TextEdit {
        TextEdit {
            start,
            end,
            text: text.to_owned(),
        }
    }

    const SRC: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n";

    #[test]
    fn an_insertion_in_a_line() {
        let edits = [edit(4, 4, "x ")];
        assert_eq!(apply(SRC, &edits), SRC.replacen("c\n", "x c\n", 1));
        assert_eq!(
            unified("f.txt", SRC, &edits),
            "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,6 +1,6 @@\n a\n b\n-c\n+x c\n d\n e\n f\n"
        );
    }

    #[test]
    fn whole_lines_and_separate_hunks() {
        // Delete "b\n" and "c\n", insert a line before "m".
        let edits = [edit(2, 6, ""), edit(24, 24, "new\n")];
        let diff = unified("f", SRC, &edits);
        assert_eq!(
            diff,
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,6 +1,4 @@\n a\n-b\n-c\n d\n e\n f\n@@ -10,5 +8,6 @@\n j\n k\n l\n+new\n m\n n\n"
        );
    }

    #[test]
    fn no_newline_at_the_end() {
        let src = "a\nb";
        let diff = unified("f", src, &[edit(3, 3, "\nc")]);
        assert_eq!(
            diff,
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\n-b\n\\ No newline at end of file\n+b\n+c\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn overlaps() {
        assert!(overlap(&edit(1, 3, ""), &edit(2, 2, "x")));
        assert!(overlap(&edit(2, 2, "x"), &edit(2, 2, "y")));
        assert!(!overlap(&edit(1, 2, ""), &edit(2, 3, "")));
    }
}
