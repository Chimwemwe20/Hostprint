use crate::style::{pad, tilde};
use crate::App;
use anyhow::Result;
use hostprint_model::format;
use serde_json::json;
use std::process::ExitCode;

pub fn run(app: &App, as_json: bool) -> Result<ExitCode> {
    let entries = app.store.list()?;
    if as_json {
        let rows: Vec<_> = entries
            .iter()
            .map(|e| match &e.info {
                Ok(info) => json!({
                    "name": e.name,
                    "path": e.path,
                    "sizeBytes": e.size,
                    "id": info.id,
                    "capturedAt": info.captured_at,
                    "hostname": info.hostname,
                    "collectorsWithData": info.collectors_with_data,
                    "collectorsTotal": info.collectors_total,
                }),
                Err(error) => json!({ "name": e.name, "path": e.path, "sizeBytes": e.size, "error": error }),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(ExitCode::SUCCESS);
    }

    let style = &app.style;
    if entries.is_empty() {
        println!(
            "No snapshots in {}. Create one with `hostprint capture --name healthy`.",
            tilde(&app.store.snapshots_dir())
        );
        return Ok(ExitCode::SUCCESS);
    }
    let name_w = entries.iter().map(|e| e.name.chars().count()).max().unwrap_or(4).max(4);
    let host_w = entries
        .iter()
        .filter_map(|e| e.info.as_ref().ok()?.hostname.as_ref().map(|h| h.chars().count()))
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{}",
        style.dim(&format!(
            "{}  {}  {}  {:>9}  COLLECTORS",
            pad("NAME", name_w),
            pad("CAPTURED", 23),
            pad("HOST", host_w),
            "SIZE"
        ))
    );
    for e in &entries {
        match &e.info {
            Ok(info) => println!(
                "{}  {}  {}  {:>9}  {}/{}",
                style.bold(&pad(&e.name, name_w)),
                info.captured_at.format("%Y-%m-%d %H:%M:%S UTC"),
                pad(info.hostname.as_deref().unwrap_or("?"), host_w),
                format::bytes(e.size),
                info.collectors_with_data,
                info.collectors_total
            ),
            Err(error) => println!("{}  {}", style.bold(&pad(&e.name, name_w)), style.red(error)),
        }
    }
    Ok(ExitCode::SUCCESS)
}
