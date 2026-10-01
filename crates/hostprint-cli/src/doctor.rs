use crate::style::{pad, tilde};
use crate::App;
use anyhow::Result;
use hostprint_collectors::docker::DockerCollector;
use hostprint_collectors::git::GitCollector;
use hostprint_collectors::redact::Redactor;
use hostprint_collectors::services::ServiceCollector;
use hostprint_collectors::system::SystemCollector;
use hostprint_collectors::{which, CaptureContext, CollectError, Collector};
use hostprint_core::Config;
use std::path::Path;
use std::process::ExitCode;

enum Check {
    Ok(String),
    Warn(String),
    Fail(String),
    /// Not applicable here; not a problem.
    Absent(String),
}

pub fn run(app: &App) -> Result<ExitCode> {
    let style = &app.style;
    // Probing collectors don't need the real fingerprint key.
    let mut ctx = CaptureContext::new(Redactor::new(b"doctor"));
    ctx.command_timeout = std::time::Duration::from_secs(3);

    let mut checks = vec![("Platform", platform(&ctx))];
    if cfg!(target_os = "macos") {
        checks.push(("System tools", macos_tools()));
    } else {
        checks.push(("/proc", proc_fs()));
    }
    checks.push(("Permissions", permissions()));
    checks.push(("Docker", probe(&DockerCollector, &ctx)));
    if cfg!(target_os = "macos") {
        checks.push(("launchd", probe(&ServiceCollector, &ctx)));
    } else {
        checks.push(("systemd", systemd()));
    }
    checks.extend([
        (
            "journalctl",
            match which("journalctl") {
                Some(p) => Check::Ok(format!("{} (journal logs with --logs-since)", p.display())),
                None => Check::Absent("not found (journal logs unavailable; Docker and file logs still work)".into()),
            },
        ),
        ("Git", git(&ctx)),
        (
            "SSH client",
            match which("ssh") {
                Some(p) => Check::Ok(format!("{} (capture ssh://host)", p.display())),
                None => Check::Absent("not found (remote capture unavailable)".into()),
            },
        ),
        ("Snapshot directory", snapshot_dir(app)),
        ("Configuration", config(app)),
    ]);

    println!("{}\n", style.bold("Hostprint Doctor"));
    for (name, check) in checks {
        let (symbol, detail) = match check {
            Check::Ok(d) => (style.green("✓"), style.dim(&d)),
            Check::Warn(d) => (style.yellow("⚠"), style.yellow(&d)),
            Check::Fail(d) => (style.red("✗"), style.red(&d)),
            Check::Absent(d) => (style.dim("–"), style.dim(&d)),
        };
        println!("  {}  {symbol}  {detail}", pad(name, 19));
    }
    Ok(ExitCode::SUCCESS)
}

fn platform(ctx: &CaptureContext) -> Check {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Check::Fail(format!(
            "{} is not supported yet; Hostprint runs on Linux and macOS",
            std::env::consts::OS
        ));
    }
    match SystemCollector.collect(ctx) {
        Ok(c) => Check::Ok(format!("{} · {}", c.summary.unwrap_or_default(), std::env::consts::ARCH)),
        Err(e) => Check::Fail(e.to_string()),
    }
}

/// The macOS tools the collectors run.
fn macos_tools() -> Check {
    let tools = [
        "/bin/ps",
        "/usr/bin/vm_stat",
        "/usr/bin/top",
        "/usr/sbin/netstat",
        "/usr/sbin/lsof",
        "/sbin/route",
        "/bin/launchctl",
    ];
    let missing: Vec<&str> = tools.iter().copied().filter(|t| !Path::new(t).exists()).collect();
    if missing.is_empty() {
        Check::Ok("ps, vm_stat, top, netstat, lsof, route, launchctl".into())
    } else {
        Check::Warn(format!("missing {}; some sections will be incomplete", missing.join(", ")))
    }
}

fn proc_fs() -> Check {
    match std::fs::read_to_string("/proc/meminfo") {
        Ok(_) => Check::Ok("readable".into()),
        Err(e) => Check::Fail(format!("cannot read /proc: {e}")),
    }
}

fn permissions() -> Check {
    match hostprint_collectors::current_user() {
        (Some(0), _) => Check::Ok("running as root: full process and socket details".into()),
        (uid, user) => Check::Warn(format!(
            "running as {} (uid {}): details of other users' processes and socket owners will be partial; root is optional",
            user.unwrap_or_else(|| "unknown".into()),
            uid.map(|u| u.to_string()).unwrap_or_else(|| "?".into())
        )),
    }
}

fn probe(collector: &dyn Collector, ctx: &CaptureContext) -> Check {
    match collector.collect(ctx) {
        Ok(c) if c.notes.is_empty() => Check::Ok(c.summary.unwrap_or_else(|| "ok".into())),
        Ok(c) => Check::Warn(c.notes.join("; ")),
        Err(CollectError::Unavailable(msg)) => Check::Absent(msg),
        Err(CollectError::Failed(msg)) => Check::Fail(msg),
    }
}

fn systemd() -> Check {
    if !Path::new("/run/systemd/system").exists() {
        return Check::Absent("not running (service collection skipped)".into());
    }
    match which("systemctl") {
        Some(_) => Check::Ok("running".into()),
        None => Check::Warn("running, but systemctl is not on PATH".into()),
    }
}

fn git(ctx: &CaptureContext) -> Check {
    if which("git").is_none() {
        return Check::Absent("not installed".into());
    }
    match GitCollector.collect(ctx) {
        Ok(c) => Check::Ok(format!("{} ({})", ctx.repo_dir.display(), c.summary.unwrap_or_default())),
        Err(CollectError::Unavailable(_)) => {
            Check::Ok("installed; current directory is not a repository (use --repo when capturing)".into())
        }
        Err(CollectError::Failed(msg)) => Check::Fail(msg),
    }
}

fn snapshot_dir(app: &App) -> Check {
    let root = app.store.root();
    if !root.exists() {
        return Check::Ok(format!("{} (created on first capture)", tilde(root)));
    }
    let count = app.store.list().map(|l| l.len()).unwrap_or(0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(root) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Check::Warn(format!(
                    "{} is accessible to other users (mode {mode:o}); run `chmod 700 {}`",
                    tilde(root),
                    root.display()
                ));
            }
        }
    }
    let probe = root.join(".doctor-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check::Ok(format!("{} · {count} snapshots", tilde(root)))
        }
        Err(e) => Check::Fail(format!("{} is not writable: {e}", tilde(root))),
    }
}

fn config(app: &App) -> Check {
    let path = app.store.config_path();
    if !path.exists() {
        return Check::Absent(format!("{} not present; using defaults", tilde(&path)));
    }
    match Config::load(&path) {
        Ok(_) => Check::Ok(tilde(&path)),
        Err(e) => Check::Fail(e.to_string()),
    }
}
