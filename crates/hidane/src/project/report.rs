//! The doctor report as text, grouped as the findings are.

use std::fmt::Write as _;

use super::{Finding, Report, Status};

/// The findings, grouped, with a mark for each status (✓ compatible or passed, · information,
/// ? not verified, ✗ incompatible), then the summary.
pub fn text(report: &Report, path: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "hidane doctor {} — {path}", report.hidane);
    let mut group = None;
    for finding in &report.findings {
        if group != Some(finding.group) {
            group = Some(finding.group);
            let _ = writeln!(out, "\n{}", finding.group.title());
        }
        write_finding(&mut out, finding);
    }
    let s = report.summary;
    let _ = writeln!(
        out,
        "\nSummary: {} compatible, {} ok, {} info, {} not verified, {} incompatible.",
        s.compatible, s.ok, s.info, s.unknown, s.incompatible
    );
    if report.has_incompatibility() {
        let _ = writeln!(
            out,
            "Exit code 1: something here does not work with hidane yet (✗)."
        );
    } else if s.unknown > 0 {
        let _ = writeln!(
            out,
            "No incompatibility found (exit code 0). Items marked ? are not verified with hidane: check them by hand."
        );
    } else {
        let _ = writeln!(out, "No incompatibility found (exit code 0).");
    }
    out
}

fn write_finding(out: &mut String, finding: &Finding) {
    let mark = match (finding.status, finding.severity) {
        (Status::Info, super::Severity::Warning) => "!",
        (status, _) => status.mark(),
    };
    let _ = writeln!(out, "  {mark} {}", finding.title);
    for detail in &finding.details {
        let _ = writeln!(out, "      {detail}");
    }
    if let Some(source) = &finding.source
        && matches!(finding.status, Status::Compatible | Status::Incompatible)
    {
        let _ = writeln!(out, "      source: {source}");
    }
}
