use crate::style::{pad, tilde, Style};
use crate::{App, CaptureArgs};
use anyhow::{bail, Result};
use hostprint_collectors::{default_collectors, Collector};
use hostprint_core::Config;
use hostprint_model::format;
use hostprint_model::{CollectorStatus, Snapshot};
use hostprint_storage::{validate_name, StorageError};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

pub fn run(app: &App, args: CaptureArgs) -> Result<ExitCode> {
    let config = app.config()?;
    let name = args.name.clone().unwrap_or_else(default_name);
    if !args.no_save {
        // Fail before spending seconds capturing.
        validate_name(&name)?;
        if app.store.exists(&name) && !args.force {
            bail!(StorageError::AlreadyExists(name));
        }
    }
    let mut ctx = app.capture_context(&config, args.repo.clone())?;
    ctx.env_files.extend(args.env_files.iter().cloned());
    ctx.file_paths.extend(args.files.iter().cloned());

    // With --json, stdout carries the snapshot and everything else goes to stderr.
    let (style, to_stderr) = if args.json { (app.err_style, true) } else { (app.style, false) };
    let say = |line: &str| {
        if to_stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let collectors = default_collectors();
    if !args.quiet {
        say(&style.bold("Capturing system state..."));
        say("");
    }
    let snapshot = hostprint_core::capture(&name, &ctx, &collectors);
    if !args.quiet {
        for line in report_lines(&snapshot, &collectors, &style) {
            say(&line);
        }
        say("");
    }

    if !args.no_save {
        let path = app.store.save(&snapshot, args.force)?;
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if args.quiet {
            if !args.json {
                println!("{name}");
            }
        } else {
            say(&format!("Snapshot saved: {}", style.bold(&name)));
            say(&style.dim(&format!(
                "  {} · {} · captured in {:.1}s",
                tilde(&path),
                format::bytes(size),
                snapshot.capture.duration_ms as f64 / 1000.0
            )));
        }
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    }
    Ok(ExitCode::SUCCESS)
}

/// Captures the live system for comparison, without saving it.
pub fn live(app: &App, config: &Config, repo: Option<PathBuf>) -> Result<Snapshot> {
    eprintln!("{}", app.err_style.dim("Capturing current state for comparison (not saved)..."));
    let ctx = app.capture_context(config, repo)?;
    Ok(hostprint_core::capture("now", &ctx, &default_collectors()))
}

/// One line per collector, in display order, plus indented notes.
pub fn report_lines(snapshot: &Snapshot, collectors: &[Arc<dyn Collector>], style: &Style) -> Vec<String> {
    let mut lines = Vec::new();
    for collector in collectors {
        let Some(report) = snapshot.collector(collector.name()) else { continue };
        let title = pad(collector.title(), 14);
        let (symbol, detail) = match report.status {
            CollectorStatus::Ok => (style.green("✓"), style.dim(report.summary.as_deref().unwrap_or(""))),
            CollectorStatus::Partial => (style.yellow("⚠"), style.dim(report.summary.as_deref().unwrap_or(""))),
            CollectorStatus::Skipped => (style.dim("–"), style.dim(report.message.as_deref().unwrap_or("skipped"))),
            CollectorStatus::Failed => (style.red("✗"), style.red(report.message.as_deref().unwrap_or("failed"))),
        };
        lines.push(format!("  {symbol} {title} {detail}").trim_end().to_string());
        for note in &report.notes {
            lines.push(format!("      {}", style.yellow(note)));
        }
    }
    lines
}

fn default_name() -> String {
    chrono::Utc::now().format("snap-%Y%m%d-%H%M%S").to_string()
}
