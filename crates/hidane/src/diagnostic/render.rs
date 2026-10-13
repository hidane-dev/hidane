//! The report as Markdown, for a GitHub issue, and as text, for a terminal. Both are made from
//! the redacted [`Report`]: nothing here reads the machine or the project.

use std::fmt::Write as _;

use super::{Report, redact::Kind};

/// The report as Markdown: a table of the build, the system and the tools, the environment
/// variables, then the project, with the doctor report and `firebase.json` folded.
pub fn markdown(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "### hidane report\n");
    let _ = writeln!(out, "<!-- {} -->", report.schema);
    let _ = writeln!(out, "> {}\n", escape_html(report.notice));
    let _ = writeln!(out, "| | |\n|---|---|");
    for (name, value) in summary(report, code, escape_html) {
        let _ = writeln!(out, "| {name} | {} |", cell(&value));
    }

    let _ = writeln!(out, "\n#### Environment variables\n");
    if report.environment.is_empty() {
        let _ = writeln!(out, "None of the variables the emulators read is set.");
    } else {
        let _ = writeln!(out, "| Variable | Value |\n|---|---|");
        for v in &report.environment {
            let _ = writeln!(
                out,
                "| {} | {} |",
                cell(&code(&v.name)),
                cell(&code(&v.value))
            );
        }
    }

    let p = &report.project;
    let _ = writeln!(out, "\n#### Project\n");
    let _ = writeln!(out, "{}\n", doctor_summary(report));
    folded(&mut out, "hidane doctor", "text", &p.doctor_text);
    for file in &p.firebase_json {
        match (&file.content, &file.error) {
            (Some(content), _) => folded(
                &mut out,
                &format!("{} (sanitized)", file.file),
                "json",
                &(serde_json::to_string_pretty(content).unwrap_or_default() + "\n"),
            ),
            (None, Some(error)) => {
                let _ = writeln!(
                    out,
                    "{} could not be read: {}\n",
                    code(&file.file),
                    escape_html(error)
                );
            }
            (None, None) => {}
        }
    }
    for rc in &p.firebaserc {
        let aliases: Vec<String> = rc
            .projects
            .iter()
            .map(|a| format!("{} → {}", code(&a.alias), code(&a.project)))
            .collect();
        let _ = writeln!(
            out,
            "{}: {}\n",
            code(&rc.file),
            if aliases.is_empty() {
                "no projects".to_owned()
            } else {
                aliases.join(", ")
            }
        );
    }

    let _ = writeln!(out, "#### Official emulator vs hidane\n");
    for paragraph in comparison(report, code, escape_html) {
        let _ = writeln!(out, "{paragraph}\n");
    }
    let _ = writeln!(out, "#### Redacted\n");
    let _ = writeln!(
        out,
        "Replaced with placeholders while the report was built (its JSON form included): {}",
        redacted(report)
    );
    out
}

/// The report as plain text.
pub fn text(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "hidane report ({})", report.schema);
    let _ = writeln!(out, "{}\n", report.notice.replace('`', ""));
    let rows = summary(report, str::to_owned, str::to_owned);
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, value) in rows {
        let _ = writeln!(out, "{name:width$}  {value}");
    }

    let _ = writeln!(out, "\nEnvironment variables");
    if report.environment.is_empty() {
        let _ = writeln!(out, "  None of the variables the emulators read is set.");
    }
    for v in &report.environment {
        let _ = writeln!(out, "  {}={}", v.name, v.value);
    }

    let p = &report.project;
    let _ = writeln!(out, "\nProject");
    let _ = writeln!(out, "  {}\n", doctor_summary(report).replace('`', ""));
    for line in p.doctor_text.lines() {
        if line.is_empty() {
            let _ = writeln!(out);
        } else {
            let _ = writeln!(out, "  {line}");
        }
    }
    for file in &p.firebase_json {
        match (&file.content, &file.error) {
            (Some(content), _) => {
                let _ = writeln!(out, "\n  {} (sanitized):", file.file);
                for line in serde_json::to_string_pretty(content)
                    .unwrap_or_default()
                    .lines()
                {
                    let _ = writeln!(out, "    {line}");
                }
            }
            (None, Some(error)) => {
                let _ = writeln!(out, "\n  {} could not be read: {error}", file.file);
            }
            (None, None) => {}
        }
    }
    for rc in &p.firebaserc {
        let aliases: Vec<String> = rc
            .projects
            .iter()
            .map(|a| format!("{} -> {}", a.alias, a.project))
            .collect();
        let _ = writeln!(out, "\n  {}: {}", rc.file, aliases.join(", "));
    }

    let _ = write!(out, "\nOfficial emulator vs hidane");
    for paragraph in comparison(report, str::to_owned, str::to_owned) {
        let _ = writeln!(out);
        for line in paragraph.lines() {
            if line.is_empty() {
                let _ = writeln!(out);
            } else {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
    let _ = writeln!(
        out,
        "\nRedacted (replaced with placeholders while the report was built, its JSON form \
         included): {}",
        redacted(report)
    );
    out
}

/// How many of the project's commands the comparison section lists.
const COMMANDS: usize = 5;

/// The comparison section's paragraphs: the report runs nothing, so it says how to compare,
/// with the project's own commands and whether the official emulator can run here.
fn comparison(
    report: &Report,
    code: impl Fn(&str) -> String,
    plain: impl Fn(&str) -> String,
) -> Vec<String> {
    let c = &report.comparison;
    let mut out = vec![format!(
        "Not compared: this report runs nothing. Please say in the issue what the same steps do \
         with the official emulator and with hidane: exit codes, errors, results. A command \
         that starts the emulators through firebase-tools uses the official emulator as it is, \
         and hidane after {}.",
        code("hidane exec --")
    )];
    if c.commands.is_empty() {
        out.push(format!(
            "No command in the project starts the Firestore emulator through firebase-tools; for \
             example: {} and {}.",
            code("firebase emulators:exec --only firestore \"npm test\""),
            code("hidane exec -- firebase emulators:exec --only firestore \"npm test\"")
        ));
    } else {
        let mut lines: Vec<String> = c
            .commands
            .iter()
            .take(COMMANDS)
            .map(|cmd| {
                format!(
                    "- {}: {}{}",
                    code(&format!("{}:{}", cmd.file, cmd.line)),
                    code(&cmd.command),
                    if cmd.under_hidane {
                        " (already under hidane exec)"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        if c.commands.len() > COMMANDS {
            lines.push(format!("- and {} more", c.commands.len() - COMMANDS));
        }
        out.push(format!(
            "The project starts the Firestore emulator with:\n\n{}",
            lines.join("\n")
        ));
    }
    let cached = &report.official_emulator_cache.firestore;
    out.push(plain(&format!(
        "The official emulator needs Java 21 or newer: {}. firebase-tools {}.",
        if c.java_for_official {
            "found here"
        } else {
            "not found here, so only hidane runs on this machine"
        },
        if cached.is_empty() {
            "downloads its jar the first time it starts it".to_owned()
        } else {
            format!("has cached v{}", cached.join(", v"))
        }
    )));
    out
}

/// The rows of the summary table: values that are code formatted by `code`, the rest by
/// `plain`.
fn summary(
    report: &Report,
    code: impl Fn(&str) -> String,
    plain: impl Fn(&str) -> String,
) -> Vec<(&'static str, String)> {
    let b = &report.hidane;
    let mut build = vec![b.version.to_owned()];
    if let Some(commit) = b.commit {
        build.push(format!("commit {}", code(&commit[..commit.len().min(12)])));
    }
    build.push(format!("{} build", b.profile));
    build.push(code(b.target));
    let mut rows = vec![("hidane", build.join(", "))];

    let s = &report.system;
    let os = match &s.os_version {
        Some(version) => format!("{} ({}, {})", plain(version), s.os, s.arch),
        None => format!("{}, {}", s.os, s.arch),
    };
    rows.push(("OS", os));
    if let Some(container) = &s.container {
        rows.push(("Container", plain(container)));
    }
    if let Some(ci) = s.ci {
        rows.push(("CI", ci.to_owned()));
    }

    let t = &report.tools;
    let java = match (&t.java.version, &t.java.runtime) {
        (Some(version), Some(runtime)) => format!("{}, {}", code(version), plain(runtime)),
        (Some(version), None) => code(version),
        _ => "not found on PATH".to_owned(),
    };
    rows.push((
        "Java",
        if t.java.hidane_exec {
            format!("{java} (the java on PATH is hidane exec's)")
        } else {
            java
        },
    ));
    rows.push((
        "Node.js",
        t.node
            .as_deref()
            .map_or_else(|| "not found on PATH".to_owned(), &code),
    ));
    let f = &t.firebase_tools;
    let mut tools = Vec::new();
    match (f.on_path, &f.path_version) {
        (true, Some(version)) => tools.push(format!("{} on PATH", plain(version))),
        (true, None) => tools.push("on PATH (version not known)".to_owned()),
        (false, _) => tools.push("not on PATH".to_owned()),
    }
    for installed in &f.installed {
        tools.push(format!(
            "{} in {}",
            plain(&installed.version),
            code(&installed.file)
        ));
    }
    rows.push(("firebase-tools", tools.join("; ")));

    let c = &report.official_emulator_cache;
    let dir = c.dir.as_deref().map(&code).unwrap_or_default();
    let cached = if !c.listed {
        format!("none (no cache directory {dir})")
    } else if c.firestore.is_empty() {
        format!("none, in {dir}")
    } else {
        let versions: Vec<String> = c
            .firestore
            .iter()
            .map(|v| format!("v{}", plain(v)))
            .collect();
        format!("{}, in {dir}", versions.join(", "))
    };
    rows.push(("Cached official emulator", cached));
    if !c.others.is_empty() {
        let others: Vec<String> = c
            .others
            .iter()
            .map(|o| format!("{} {}", o.emulator, plain(&o.version)))
            .collect();
        rows.push(("Other cached emulators", others.join(", ")));
    }
    rows
}

fn doctor_summary(report: &Report) -> String {
    let s = &report.project.doctor["summary"];
    let n = |key: &str| s[key].as_u64().unwrap_or(0);
    format!(
        "`hidane doctor`: {} compatible, {} likely compatible, {} ok, {} info, {} not verified, {} incompatible.",
        n("compatible"),
        n("likely_compatible"),
        n("ok"),
        n("info"),
        n("unknown"),
        n("incompatible"),
    )
}

fn redacted(report: &Report) -> String {
    let parts: Vec<String> = report
        .redacted
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(kind, n)| {
            let (one, many) = Kind::noun(*kind);
            format!("{n} {}", if *n == 1 { one } else { many })
        })
        .collect();
    if parts.is_empty() {
        "nothing.".to_owned()
    } else {
        format!("{}.", parts.join(", "))
    }
}

/// A `<details>` block holding a fenced code block.
fn folded(out: &mut String, summary: &str, lang: &str, content: &str) {
    let fence = fence(content);
    let _ = writeln!(
        out,
        "<details>\n<summary>{}</summary>\n\n{fence}{lang}\n{content}{fence}\n\n</details>\n",
        escape_html(summary)
    );
}

/// Backticks enough to fence `content`: more than its longest run.
fn fence(content: &str) -> String {
    let longest = content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat((longest + 1).max(3))
}

/// Inline code that holds any text.
fn code(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    if longest == 0 {
        format!("{ticks}{text}{ticks}")
    } else {
        format!("{ticks} {text} {ticks}")
    }
}

/// A table cell: one line, pipes escaped.
fn cell(text: &str) -> String {
    text.replace('\n', " ").replace('|', "\\|")
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_holds_any_text() {
        assert_eq!(fence("a ``` b"), "````");
        assert_eq!(fence("plain"), "```");
        assert_eq!(code("x"), "`x`");
        assert_eq!(code("a`b"), "`` a`b ``");
        assert_eq!(cell("a|b\nc"), "a\\|b c");
        assert_eq!(escape_html("<project>"), "&lt;project&gt;");
    }
}
