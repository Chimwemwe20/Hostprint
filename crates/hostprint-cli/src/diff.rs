use crate::style::{clip, pad, Style};
use crate::{capture, App, DiffArgs};
use anyhow::Result;
use hostprint_diff::{Category, Change, ChangeKind, Diff, Significance};
use hostprint_model::format;
use std::process::ExitCode;

const MAX_VALUE: usize = 60;
const MAX_SUBJECT: usize = 32;
const MAX_FIELD: usize = 26;

pub fn run(app: &App, args: DiffArgs) -> Result<ExitCode> {
    let config = app.config()?;
    let from = app.store.resolve(&args.from)?;
    let to = match &args.to {
        Some(reference) => app.store.resolve(reference)?,
        None => capture::live(app, &config, args.repo.clone())?,
    };
    let diff = hostprint_diff::diff(&from, &to, &App::diff_options(&config));

    if args.json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        let min = if args.all { Significance::Info } else { args.min.into() };
        print!("{}", render(&diff, min, &app.style));
    }
    let failed =
        args.fail_on.map(Significance::from).is_some_and(|threshold| diff.highest().is_some_and(|h| h >= threshold));
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

pub fn render(diff: &Diff, min: Significance, style: &Style) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };

    line(style.bold("HOSTPRINT DIFF"));
    line(String::new());
    let width = diff.from.name.chars().count().max(diff.to.name.chars().count());
    let header = |name: &str, at: chrono::DateTime<chrono::Utc>, host: &Option<String>| {
        format!(
            "{}  {}{}",
            style.bold(&pad(name, width)),
            style.dim(&at.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
            host.as_ref().map(|h| style.dim(&format!("  {h}"))).unwrap_or_default()
        )
    };
    let elapsed = (diff.to.captured_at - diff.from.captured_at).num_seconds();
    let gap = if elapsed >= 0 {
        format!("{} later", format::duration(elapsed as u64))
    } else {
        format!("{} earlier", format::duration(elapsed.unsigned_abs()))
    };
    line(format!("  {}", header(&diff.from.name, diff.from.captured_at, &diff.from.hostname)));
    line(format!(
        "→ {}  {}",
        header(&diff.to.name, diff.to.captured_at, &diff.to.hostname),
        style.dim(&format!("({gap})"))
    ));
    line(String::new());

    for note in &diff.notes {
        line(format!("  {} {}", style.yellow("⚠"), note));
    }
    if !diff.notes.is_empty() {
        line(String::new());
    }

    let shown: Vec<&Change> = diff.at_least(min).collect();
    let hidden = diff.changes.len() - shown.len();
    if shown.is_empty() {
        let mut msg = if diff.changes.is_empty() {
            "No differences found.".to_string()
        } else {
            format!("No changes at {} or above.", min.label())
        };
        if hidden > 0 {
            msg.push_str(&style.dim(&format!("  ({hidden} lower-significance changes hidden; --all to show)")));
        }
        line(msg);
        return out;
    }

    let count = |sig: Significance| shown.iter().filter(|c| c.significance == sig).count();
    let parts: Vec<String> = [Significance::High, Significance::Medium, Significance::Low, Significance::Info]
        .into_iter()
        .filter_map(|sig| {
            let n = count(sig);
            (n > 0).then(|| format!("{n} {}", sig.label().to_lowercase()))
        })
        .collect();
    let mut summary =
        format!("{} {}: {}", shown.len(), if shown.len() == 1 { "change" } else { "changes" }, parts.join(" · "));
    if hidden > 0 {
        summary.push_str(&style.dim(&format!("   ({hidden} lower-significance hidden; --all to show)")));
    }
    line(style.bold(&summary));

    for sig in [Significance::High, Significance::Medium, Significance::Low, Significance::Info] {
        let group: Vec<&&Change> = shown.iter().filter(|c| c.significance == sig).collect();
        if group.is_empty() {
            continue;
        }
        line(String::new());
        line(level(sig, style));
        let subject_w = group.iter().map(|c| c.subject.chars().count()).max().unwrap_or(0).min(MAX_SUBJECT);
        let field_w = group
            .iter()
            .map(|c| c.field.as_deref().map(|f| f.chars().count()).unwrap_or(0))
            .max()
            .unwrap_or(0)
            .min(MAX_FIELD);
        let mut category: Option<Category> = None;
        for c in group {
            if category != Some(c.category) {
                category = Some(c.category);
                line(format!("  {}", style.dim(c.category.label())));
            }
            let value = match c.kind {
                ChangeKind::Changed => format!(
                    "{} → {}",
                    clip(c.before.as_deref().unwrap_or(""), MAX_VALUE),
                    style.bold(&clip(c.after.as_deref().unwrap_or(""), MAX_VALUE))
                ),
                ChangeKind::Added => style.green(&format!("+ {}", clip(c.after.as_deref().unwrap_or(""), MAX_VALUE))),
                ChangeKind::Removed => style.red(&format!("− {}", clip(c.before.as_deref().unwrap_or(""), MAX_VALUE))),
            };
            let delta = c.delta.as_ref().map(|d| style.dim(&format!("  ({d})"))).unwrap_or_default();
            let subject = pad(&clip(&c.subject, MAX_SUBJECT), subject_w);
            let field = pad(&clip(c.field.as_deref().unwrap_or(""), MAX_FIELD), field_w);
            line(format!("    {}  {}  {value}{delta}", style.bold(&subject), style.dim(&field)));
        }
    }
    out
}

fn level(sig: Significance, style: &Style) -> String {
    match sig {
        Significance::High => style.red_bold("HIGH"),
        Significance::Medium => style.yellow_bold("MEDIUM"),
        Significance::Low => style.blue_bold("LOW"),
        Significance::Info => style.dim("INFO"),
    }
}
