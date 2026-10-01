//! `hostprint baseline` and `hostprint check`: a named known-good state, and a
//! one-step comparison of the live system against it.

use crate::style::tilde;
use crate::{capture, diff, list, show, App, BaselineCommand, CheckArgs, Format};
use anyhow::{bail, Result};
use hostprint_diff::Significance;
use hostprint_storage::{validate_name, Kind, StorageError};
use std::process::ExitCode;

pub fn run(app: &App, command: BaselineCommand) -> Result<ExitCode> {
    match command {
        BaselineCommand::Create { name, from, force, options } => {
            validate_name(&name)?;
            if app.store.exists_in(Kind::Baseline, &name) && !force {
                bail!(StorageError::AlreadyExists { kind: Kind::Baseline, name });
            }
            let (mut snapshot, origin) = match &from {
                Some(reference) => (app.store.resolve(reference)?, format!("from snapshot '{reference}'")),
                None => {
                    let config = app.config()?;
                    (capture::live(app, &config, &options, &name, "for the baseline")?, "captured now".to_string())
                }
            };
            snapshot.name = name.clone();
            let path = app.store.save_in(Kind::Baseline, &snapshot, force)?;
            println!("Baseline saved: {} ({origin})", app.style.bold(&name));
            println!("{}", app.style.dim(&format!("  {}", tilde(&path))));
            println!("Compare the system with it any time: hostprint check {name}");
            Ok(ExitCode::SUCCESS)
        }
        BaselineCommand::List { json } => list::run(app, Kind::Baseline, json),
        BaselineCommand::Show { name, json } => {
            show::print(&app.store.load_from(Kind::Baseline, &name)?, None, json, &app.style)
        }
        BaselineCommand::Delete { name } => {
            let path = app.store.delete_from(Kind::Baseline, &name)?;
            println!("Deleted baseline {} ({})", name, tilde(&path));
            Ok(ExitCode::SUCCESS)
        }
    }
}

pub fn check(app: &App, args: CheckArgs) -> Result<ExitCode> {
    let config = app.config()?;
    let baseline = app.store.load_from(Kind::Baseline, &args.baseline)?;
    let opts = app.diff_options(&config)?;
    let purpose = format!("to check against baseline '{}'", args.baseline);
    let now = capture::live(app, &config, &args.options, "now", &purpose)?;
    let result = hostprint_diff::diff(&baseline, &now, &opts);
    let threshold: Significance = args.fail_on.into();

    let text = args.output.format() == Format::Text;
    if text {
        println!("Checking against the {} baseline...\n", app.style.bold(&args.baseline));
    }
    diff::print(&result, &args.output, &app.style)?;
    if text {
        let failing = result.at_least(threshold).count();
        println!();
        if failing > 0 {
            let what = if failing == 1 { "change" } else { "changes" };
            println!("{} {failing} {what} at {} or above", app.style.red_bold("✗"), threshold.label());
        } else {
            println!("{} Nothing at {} or above", app.style.green("✓"), threshold.label());
        }
    }
    Ok(diff::exit_code(&result, Some(threshold)))
}
