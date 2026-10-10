//! Document and collection paths, relative to `projects/{p}/databases/{d}/documents`.

use std::fmt;

/// A slash-separated path: an even number of segments names a document (`users/alice`), an
/// odd number a collection (`users`, `users/alice/posts`). The empty path is the database root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ResourcePath {
    segments: Vec<String>,
}

impl ResourcePath {
    pub fn root() -> Self {
        Self::default()
    }

    /// Splits on `/`. Returns `None` for empty segments (`a//b`, a leading or trailing `/`).
    pub fn parse(path: &str) -> Option<Self> {
        if path.is_empty() {
            return Some(Self::root());
        }
        let segments: Vec<String> = path.split('/').map(str::to_owned).collect();
        segments
            .iter()
            .all(|s| !s.is_empty())
            .then_some(Self { segments })
    }

    pub fn from_segments<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            segments: segments.into_iter().map(Into::into).collect(),
        }
    }

    pub fn segments(&self) -> impl ExactSizeIterator<Item = &str> + Clone {
        self.segments.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn is_document(&self) -> bool {
        !self.segments.is_empty() && self.segments.len().is_multiple_of(2)
    }

    pub fn is_collection(&self) -> bool {
        !self.segments.len().is_multiple_of(2)
    }

    /// The last segment: a document ID or a collection ID.
    pub fn last(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }

    /// The ID of the collection this document belongs to.
    pub fn collection_id(&self) -> Option<&str> {
        if self.is_document() {
            self.segments
                .get(self.segments.len() - 2)
                .map(String::as_str)
        } else {
            None
        }
    }

    pub fn parent(&self) -> Option<Self> {
        (!self.segments.is_empty()).then(|| Self {
            segments: self.segments[..self.segments.len() - 1].to_vec(),
        })
    }

    pub fn child(&self, segment: impl Into<String>) -> Self {
        let mut segments = self.segments.clone();
        segments.push(segment.into());
        Self { segments }
    }

    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.segments.starts_with(&prefix.segments)
    }
}

impl fmt::Display for ResourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.segments.join("/"))
    }
}
