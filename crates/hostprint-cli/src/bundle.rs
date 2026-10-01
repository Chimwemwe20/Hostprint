//! `hostprint bundle`: one archive with everything needed to discuss an
//! incident: the snapshot, an optional baseline and diff, a Markdown report
//! and the collected logs, plus checksums.
//!
//! ```text
//! incident-20261001-143646/
//!   manifest.json      what is in the bundle and where it came from
//!   snapshot.json      the snapshot (the documented snapshot format)
//!   baseline.json      the snapshot it was compared with (with --against)
//!   diff.json          the comparison (with --against)
//!   report.md          human-readable summary (Markdown)
//!   report.html        the same, as a standalone page
//!   logs/*.log         collected log lines, one file per source
//!   checksums.sha256   `sha256sum -c` compatible
//! ```

use crate::style::tilde;
use crate::{capture, html, report, App, BundleArgs};
use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use hostprint_collectors::redact::hex;
use hostprint_diff::Diff;
use hostprint_model::format::bytes;
use hostprint_model::Snapshot;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Version of the bundle layout.
const BUNDLE_VERSION: u32 = 1;

pub fn run(app: &App, args: BundleArgs) -> Result<ExitCode> {
    let config = app.config()?;
    let snapshot = match &args.snapshot {
        Some(reference) => app.store.resolve(reference)?,
        None => {
            let name = capture::default_name().replacen("snap-", "support-", 1);
            capture::live(app, &config, &args.options, &name, "for the bundle")?
        }
    };
    let baseline = args.against.as_deref().map(|r| app.store.resolve(r)).transpose()?;
    let diff = baseline.as_ref().map(|b| hostprint_diff::diff(b, &snapshot, &App::diff_options(&config)));

    let written = write(&snapshot, baseline.as_ref(), diff.as_ref(), args.output)?;
    let (output, root, files) = (written.path, written.root, written.files);
    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    println!("Bundle written: {} ({})", app.style.bold(&tilde(&output)), bytes(size));
    for (path, data) in &files {
        println!("{}", app.style.dim(&format!("  {root}/{path}  {}", bytes(data.len() as u64))));
    }
    if let Some(d) = &diff {
        let s = d.summary;
        println!("Diff against '{}': {} high · {} medium · {} low", d.from.name, s.high, s.medium, s.low);
    }
    eprintln!(
        "{}",
        app.err_style.dim(
            "Secrets are redacted, but the bundle describes your system (hostnames, addresses, command lines, log \
             lines). Review it before sharing outside your team."
        )
    );
    Ok(ExitCode::SUCCESS)
}

pub struct Written {
    pub path: PathBuf,
    /// The archive's top-level directory.
    pub root: String,
    pub files: Vec<(String, Vec<u8>)>,
}

/// Writes a bundle to `output`, or to `<name>-<timestamp>.tar.gz` in the
/// current directory. Never overwrites.
pub fn write(
    snapshot: &Snapshot,
    baseline: Option<&Snapshot>,
    diff: Option<&Diff>,
    output: Option<PathBuf>,
) -> Result<Written> {
    let root = format!("{}-{}", snapshot.name, snapshot.captured_at.format("%Y%m%d-%H%M%S"));
    let path = output.unwrap_or_else(|| PathBuf::from(format!("{root}.tar.gz")));
    if path.exists() {
        bail!("{} already exists (choose another path with --output)", path.display());
    }
    let files = contents(snapshot, baseline, diff)?;
    write_archive(&path, &root, &files).with_context(|| format!("writing {}", path.display()))?;
    Ok(Written { path, root, files })
}

/// The files of the bundle, in archive order, ending with the manifest and
/// the checksums that cover everything before them.
pub fn contents(
    snapshot: &Snapshot,
    baseline: Option<&Snapshot>,
    diff: Option<&Diff>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files: Vec<(String, Vec<u8>)> = vec![("snapshot.json".into(), pretty(snapshot)?)];
    if let Some(b) = baseline {
        files.push(("baseline.json".into(), pretty(b)?));
    }
    if let Some(d) = diff {
        files.push(("diff.json".into(), pretty(d)?));
    }

    files.push(("report.md".into(), report::combined_markdown(snapshot, diff).into_bytes()));
    files.push(("report.html".into(), html::report(snapshot, diff).into_bytes()));

    if let Some(logs) = &snapshot.logs {
        let mut used = std::collections::HashSet::new();
        for src in &logs.sources {
            let mut name = format!("logs/{}-{}.log", src.kind, sanitize(&src.name));
            let mut n = 2;
            while !used.insert(name.clone()) {
                name = format!("logs/{}-{}-{n}.log", src.kind, sanitize(&src.name));
                n += 1;
            }
            let mut text = format!(
                "# {} ({}) since {}: {} lines, {} errors, {} warnings{}\n",
                src.name,
                src.kind,
                logs.since.format("%Y-%m-%dT%H:%M:%SZ"),
                src.total,
                src.errors,
                src.warnings,
                if src.truncated { "; older lines not kept" } else { "" }
            );
            for line in &src.lines {
                text.push_str(line);
                text.push('\n');
            }
            files.push((name, text.into_bytes()));
        }
    }

    let manifest = json!({
        "bundleVersion": BUNDLE_VERSION,
        "createdAt": chrono::Utc::now(),
        "hostprintVersion": env!("CARGO_PKG_VERSION"),
        "snapshot": reference(snapshot),
        "baseline": baseline.map(reference),
        "diffSummary": diff.map(|d| d.summary),
        "files": files.iter().map(|(path, data)| json!({
            "path": path,
            "bytes": data.len(),
            "sha256": sha256(data),
        })).collect::<Vec<_>>(),
    });
    files.push(("manifest.json".into(), serde_json::to_vec_pretty(&manifest)?));
    let checksums: String = files.iter().map(|(path, data)| format!("{}  {path}\n", sha256(data))).collect();
    files.push(("checksums.sha256".into(), checksums.into_bytes()));
    Ok(files)
}

fn reference(s: &Snapshot) -> serde_json::Value {
    json!({ "name": s.name, "id": s.id, "capturedAt": s.captured_at, "hostname": s.hostname() })
}

fn write_archive(output: &Path, root: &str, files: &[(String, Vec<u8>)]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(output)?;
    let mut archive = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    let mtime = chrono::Utc::now().timestamp().max(0) as u64;
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(mtime);
        header.set_entry_type(tar::EntryType::Regular);
        archive.append_data(&mut header, format!("{root}/{path}"), data.as_slice())?;
    }
    archive.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

fn sha256(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// A file-name-safe version of a log source name such as `nginx.service` or `/var/log/app.log`.
fn sanitize(name: &str) -> String {
    let s: String = name
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    if s.is_empty() {
        "unnamed".into()
    } else {
        s
    }
}

fn pretty<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut v = serde_json::to_vec_pretty(value)?;
    v.push(b'\n');
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("/var/log/app.log"), "var_log_app.log");
        assert_eq!(sanitize("nginx.service"), "nginx.service");
        assert_eq!(sanitize("///"), "unnamed");
    }
}
