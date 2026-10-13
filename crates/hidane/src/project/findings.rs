//! What a scan concludes, one finding per check.

use serde::Serialize;

/// Whether hidane is known to work for the thing a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Verified with hidane; the finding's `source` says where. For a version, its major and
    /// minor versions are the verified one's.
    Compatible,
    /// The same major version as one verified with hidane, but not that version itself; the
    /// finding's `source` names the verification. Not an incompatibility.
    LikelyCompatible,
    /// A check that passed, which claims nothing about verification (`firebase.json` found).
    Ok,
    /// Information: nothing to verify, or nothing hidane changes.
    Info,
    /// Nothing verifies it: manual verification required.
    Unknown,
    /// A known gap: it will not work with hidane as it is.
    Incompatible,
}

impl Status {
    pub fn mark(self) -> &'static str {
        match self {
            Status::Compatible | Status::Ok => "✓",
            Status::LikelyCompatible => "~",
            Status::Info => "·",
            Status::Unknown => "?",
            Status::Incompatible => "✗",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// The part of the project a finding is about, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    Firebase,
    Rules,
    Functions,
    Dependencies,
    Workflows,
    Scripts,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Firebase => "Firebase configuration",
            Group::Rules => "Security Rules",
            Group::Functions => "Cloud Functions",
            Group::Dependencies => "SDKs and tools",
            Group::Workflows => "CI workflows",
            Group::Scripts => "npm scripts and shell scripts",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// A stable identifier of the check (`rules.compile`, `workflow.setup-java`, …).
    pub code: &'static str,
    pub group: Group,
    pub status: Status,
    pub severity: Severity,
    /// One line.
    pub title: String,
    /// More lines, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    /// What backs a `compatible` or `incompatible` status: a document, a result file, an
    /// issue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

impl Finding {
    fn new(
        code: &'static str,
        group: Group,
        status: Status,
        severity: Severity,
        title: String,
    ) -> Self {
        Self {
            code,
            group,
            status,
            severity,
            title,
            details: Vec::new(),
            source: None,
            file: None,
            line: None,
        }
    }

    /// Verified with hidane, as `source` shows.
    pub fn compatible(
        code: &'static str,
        group: Group,
        title: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self::new(
            code,
            group,
            Status::Compatible,
            Severity::Info,
            title.into(),
        )
        .with_source(source)
    }

    /// The same major version as a verified one, which `source` shows.
    pub fn likely_compatible(
        code: &'static str,
        group: Group,
        title: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self::new(
            code,
            group,
            Status::LikelyCompatible,
            Severity::Info,
            title.into(),
        )
        .with_source(source)
    }

    pub fn ok(code: &'static str, group: Group, title: impl Into<String>) -> Self {
        Self::new(code, group, Status::Ok, Severity::Info, title.into())
    }

    pub fn info(code: &'static str, group: Group, title: impl Into<String>) -> Self {
        Self::new(code, group, Status::Info, Severity::Info, title.into())
    }

    /// Information the user should act on.
    pub fn note(code: &'static str, group: Group, title: impl Into<String>) -> Self {
        Self::new(code, group, Status::Info, Severity::Warning, title.into())
    }

    /// Not verified: manual verification required.
    pub fn unknown(code: &'static str, group: Group, title: impl Into<String>) -> Self {
        Self::new(
            code,
            group,
            Status::Unknown,
            Severity::Warning,
            title.into(),
        )
    }

    /// A known gap, as `source` shows.
    pub fn incompatible(
        code: &'static str,
        group: Group,
        title: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self::new(
            code,
            group,
            Status::Incompatible,
            Severity::Error,
            title.into(),
        )
        .with_source(source)
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn at(mut self, file: &str, line: Option<usize>) -> Self {
        self.file = Some(file.to_owned());
        self.line = line;
        self
    }

    pub fn detail(mut self, line: impl Into<String>) -> Self {
        self.details.push(line.into());
        self
    }

    pub fn details(mut self, lines: impl IntoIterator<Item = String>) -> Self {
        self.details.extend(lines);
        self
    }

    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}

/// How many findings have each status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub compatible: usize,
    pub likely_compatible: usize,
    pub ok: usize,
    pub info: usize,
    pub unknown: usize,
    pub incompatible: usize,
}

impl Summary {
    pub fn of(findings: &[Finding]) -> Self {
        let mut summary = Self::default();
        for finding in findings {
            *match finding.status {
                Status::Compatible => &mut summary.compatible,
                Status::LikelyCompatible => &mut summary.likely_compatible,
                Status::Ok => &mut summary.ok,
                Status::Info => &mut summary.info,
                Status::Unknown => &mut summary.unknown,
                Status::Incompatible => &mut summary.incompatible,
            } += 1;
        }
        summary
    }
}
