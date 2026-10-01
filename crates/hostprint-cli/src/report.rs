//! Markdown reports, for `diff --format markdown` and incident bundles. The
//! output renders well on GitHub, GitLab and most ticketing systems.

use crate::show;
use crate::style::{plural, Style};
use hostprint_diff::{Change, ChangeKind, Diff, Significance};
use hostprint_model::{CollectorStatus, Snapshot};
use std::fmt::Write;

/// Lines of each log source included in a report.
const LOG_LINES: usize = 20;

pub fn diff_markdown(diff: &Diff, min: Significance) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Hostprint diff: {} → {}\n", code(&diff.from.name), code(&diff.to.name));
    md.push_str("| | Snapshot | Captured | Host |\n| --- | --- | --- | --- |\n");
    for (label, s) in [("From", &diff.from), ("To", &diff.to)] {
        let _ = writeln!(
            md,
            "| {label} | {} | {} | {} |",
            code(&s.name),
            s.captured_at.format("%Y-%m-%d %H:%M:%S UTC"),
            s.hostname.as_deref().map(code).unwrap_or_default()
        );
    }
    md.push('\n');

    let shown: Vec<&Change> = diff.at_least(min).collect();
    let hidden = diff.changes.len() - shown.len();
    if diff.changes.is_empty() {
        md.push_str("**No differences found.**\n\n");
    } else {
        let counts: Vec<String> = [Significance::High, Significance::Medium, Significance::Low, Significance::Info]
            .into_iter()
            .filter_map(|sig| {
                let n = diff.changes.iter().filter(|c| c.significance == sig).count();
                (n > 0).then(|| format!("{n} {}", sig.label().to_lowercase()))
            })
            .collect();
        let _ = writeln!(md, "**{} changes:** {}\n", diff.changes.len(), counts.join(" · "));
    }
    for note in &diff.notes {
        let _ = writeln!(md, "> **Note:** {}\n", escape(note));
    }

    for sig in [Significance::High, Significance::Medium, Significance::Low, Significance::Info] {
        let group: Vec<&&Change> = shown.iter().filter(|c| c.significance == sig).collect();
        if group.is_empty() {
            continue;
        }
        // INFO is expected churn: present, but folded away.
        if sig == Significance::Info {
            let _ = writeln!(md, "<details>\n<summary>{} INFO changes</summary>\n", group.len());
        } else {
            let _ = writeln!(md, "## {}\n", sig.label());
        }
        md.push_str("| Category | Subject | Field | Before | After | Change | Rule |\n");
        md.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        for c in group {
            let (before, after) = match c.kind {
                ChangeKind::Added => ("—".to_string(), cell(c.after.as_deref())),
                ChangeKind::Removed => (cell(c.before.as_deref()), "—".to_string()),
                ChangeKind::Changed => (cell(c.before.as_deref()), cell(c.after.as_deref())),
            };
            let _ = writeln!(
                md,
                "| {} | {} | {} | {before} | {after} | {} | {} |",
                title_case(c.category.label()),
                escape(&c.subject),
                escape(c.field.as_deref().unwrap_or("")),
                escape(c.delta.as_deref().unwrap_or("")),
                code(&c.rule)
            );
        }
        if sig == Significance::Info {
            md.push_str("\n</details>\n");
        }
        md.push('\n');
    }
    if hidden > 0 {
        let _ = writeln!(md, "_{} not shown._\n", plural(hidden as u64, "lower-significance change"));
    }
    md.push_str("Significance comes from fixed rules; each row names its rule. Changes are evidence, not causes.\n");
    md
}

pub fn snapshot_markdown(s: &Snapshot) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Hostprint snapshot: {}\n", code(&s.name));
    let _ = writeln!(
        md,
        "Captured {} on {} by {} with Hostprint {}.\n",
        s.captured_at.format("%Y-%m-%d %H:%M:%S UTC"),
        code(s.hostname().unwrap_or("unknown host")),
        code(s.capture.user.as_deref().unwrap_or("unknown user")),
        s.capture.hostprint_version
    );
    md.push_str("```text\n");
    for line in show::overview(s, &Style::plain()) {
        md.push_str(&line);
        md.push('\n');
    }
    md.push_str("```\n\n## Collection\n\n| Collector | Status | Detail |\n| --- | --- | --- |\n");
    for c in &s.capture.collectors {
        let status = match c.status {
            CollectorStatus::Ok => "ok",
            CollectorStatus::Partial => "partial",
            CollectorStatus::Skipped => "skipped",
            CollectorStatus::Failed => "**failed**",
        };
        let mut detail: Vec<String> = c.summary.iter().chain(&c.message).cloned().collect();
        detail.extend(c.notes.iter().cloned());
        let _ = writeln!(md, "| {} | {status} | {} |", c.name, escape(&detail.join("; ")));
    }

    if let Some(logs) = &s.logs {
        let _ = writeln!(md, "\n## Logs since {}\n", logs.since.format("%Y-%m-%d %H:%M:%S UTC"));
        let mut sources: Vec<_> = logs.sources.iter().collect();
        sources.sort_by(|a, b| b.errors.cmp(&a.errors).then(a.name.cmp(&b.name)));
        for src in sources {
            let _ = writeln!(
                md,
                "### {} ({})\n\n{} · {} · {}{}\n",
                escape(&src.name),
                src.kind,
                plural(src.total, "line"),
                plural(src.errors, "error"),
                plural(src.warnings, "warning"),
                if src.truncated { " · truncated" } else { "" }
            );
            if !src.top_errors.is_empty() {
                md.push_str("| Error | Count |\n| --- | --- |\n");
                for p in &src.top_errors {
                    let _ = writeln!(md, "| {} | {} |", cell(Some(&p.example)), p.count);
                }
                md.push('\n');
            }
            let start = src.lines.len().saturating_sub(LOG_LINES);
            md.push_str("```text\n");
            for line in &src.lines[start..] {
                md.push_str(&line.replace("```", "'''"));
                md.push('\n');
            }
            md.push_str("```\n\n");
        }
    }
    md
}

/// An inline code span safe inside a table cell.
fn code(s: &str) -> String {
    format!("`{}`", s.replace('`', "'").replace('|', "\\|"))
}

fn cell(value: Option<&str>) -> String {
    match value {
        Some(v) if !v.is_empty() => code(v),
        _ => "—".to_string(),
    }
}

/// Escapes text that should not be read as Markdown.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '|' | '*' | '_' | '`' | '<' | '>' | '[' | ']' | '#') {
            out.push('\\');
        }
        out.push(if c == '\n' { ' ' } else { c });
    }
    out
}

fn title_case(label: &str) -> String {
    let lower = label.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markdown() {
        assert_eq!(escape("a|b *c* <d>"), "a\\|b \\*c\\* \\<d\\>");
        assert_eq!(code("x|`y`"), "`x\\|'y'`");
        assert_eq!(cell(Some("")), "—");
        assert_eq!(title_case("CONTAINERS"), "Containers");
    }
}
