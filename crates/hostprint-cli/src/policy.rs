//! `hostprint policy show` and `hostprint policy rules`.

use crate::style::pad;
use crate::{App, PolicyCommand};
use anyhow::Result;
use hostprint_diff::policy::RULES;
use hostprint_diff::{Action, Thresholds};
use std::process::ExitCode;

pub fn run(app: &App, command: PolicyCommand) -> Result<ExitCode> {
    let style = &app.style;
    match command {
        PolicyCommand::Rules => {
            let width = RULES.iter().map(|(id, _)| id.len()).max().unwrap_or(0);
            for (id, what) in RULES {
                println!("{}  {}", style.bold(&pad(id, width)), style.dim(what));
            }
            println!();
            println!("{}", style.dim("Levels and thresholds for each rule: docs/diff-rules.md"));
        }
        PolicyCommand::Show => {
            let config = app.config()?;
            let policy = app.policy(&config)?;
            let mut sources = vec![app.store.config_path().display().to_string()];
            sources.extend(app.policy_file.iter().map(|p| p.display().to_string()));
            println!("{} {}", style.bold("Policy from"), sources.join(" + "));
            println!();
            if policy.rules.is_empty() {
                println!("  {}", style.dim("No rule overrides: every rule uses its built-in level."));
            } else {
                println!("{}", style.dim("  Rules, first match wins:"));
                let unknown = policy.unknown_rules();
                for r in &policy.rules {
                    let level = match r.action {
                        Action::Off => style.dim("off"),
                        Action::Level(s) => style.bold(s.label()),
                    };
                    let subject = r.subject.as_ref().map(|s| format!(" for {s}")).unwrap_or_default();
                    let warning = if unknown.iter().any(|u| std::ptr::eq(*u, r)) {
                        style.yellow("  ⚠ matches no rule (see `hostprint policy rules`)")
                    } else {
                        String::new()
                    };
                    println!("  {}{subject} → {level}  {}{warning}", r.rule, style.dim(&format!("({})", r.source)));
                }
            }
            println!();
            let t = policy.thresholds;
            let d = Thresholds::default();
            let mark = |v: f64, default: f64| if (v - default).abs() > f64::EPSILON { "" } else { "  (default)" };
            println!("{}", style.dim("  Thresholds:"));
            println!(
                "  disk usage HIGH at        {:.0}%{}",
                t.disk_high * 100.0,
                style.dim(mark(t.disk_high, d.disk_high))
            );
            println!(
                "  disk usage MEDIUM at      {:.0}%{}",
                t.disk_medium * 100.0,
                style.dim(mark(t.disk_medium, d.disk_medium))
            );
            println!(
                "  memory available HIGH at  below {:.0}% of total{}",
                t.memory_available_high * 100.0,
                style.dim(mark(t.memory_available_high, d.memory_available_high))
            );
            println!(
                "  load HIGH at              {:.1} per core{}",
                t.load_high,
                style.dim(mark(t.load_high, d.load_high))
            );
            println!(
                "  load MEDIUM at            {:.1} per core{}",
                t.load_medium,
                style.dim(mark(t.load_medium, d.load_medium))
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
