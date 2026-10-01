use crate::style::{clip, pad, plural, Style};
use crate::{App, ShowArgs};
use anyhow::{bail, Result};
use clap::ValueEnum;
use hostprint_model::format::{bytes, duration};
use hostprint_model::{CollectorStatus, Snapshot};
use std::process::ExitCode;

const TOP_PROCESSES: usize = 10;

#[derive(Clone, Copy, ValueEnum)]
pub enum SectionArg {
    Processes,
    Ports,
    Interfaces,
    Disks,
    Services,
    Containers,
    Env,
    Runtimes,
    Files,
    Logs,
    Collectors,
}

pub fn run(app: &App, args: ShowArgs) -> Result<ExitCode> {
    let snapshot = app.store.resolve(&args.snapshot)?;
    print(&snapshot, args.section, args.json, &app.style)
}

pub fn print(snapshot: &Snapshot, section: Option<SectionArg>, json: bool, style: &Style) -> Result<ExitCode> {
    if json {
        println!("{}", serde_json::to_string_pretty(snapshot)?);
        return Ok(ExitCode::SUCCESS);
    }
    let lines = match section {
        None => overview(snapshot, style),
        Some(section) => full_section(snapshot, section, style)?,
    };
    for line in lines {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

fn heading(style: &Style, title: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("\n{}", style.bold(title))
    } else {
        format!("\n{}  {}", style.bold(title), style.dim(detail))
    }
}

fn row(style: &Style, label: &str, value: impl AsRef<str>) -> String {
    format!("  {}  {}", style.dim(&pad(label, 10)), value.as_ref())
}

pub fn overview(s: &Snapshot, style: &Style) -> Vec<String> {
    let mut out = vec![format!("{} {}", style.bold("SNAPSHOT"), style.bold(&s.name))];
    out.push(row(style, "id", &s.id));
    let age = (chrono::Utc::now() - s.captured_at).num_seconds().max(0) as u64;
    out.push(row(
        style,
        "captured",
        format!("{} ({} ago)", s.captured_at.format("%Y-%m-%d %H:%M:%S UTC"), duration(age)),
    ));
    let user = s.capture.user.as_deref().unwrap_or("unknown");
    let privilege = match (s.capture.elevated, user) {
        (true, "root") => "",
        (true, _) => " (as root)",
        (false, _) => " (not root: other users' details may be partial)",
    };
    out.push(row(style, "by", format!("{user}{privilege}")));
    if let Some(remote) = &s.capture.remote {
        out.push(row(style, "via", remote));
    }

    if let Some(h) = &s.host {
        let mut parts = vec![h.hostname.clone()];
        parts.extend(h.os.as_ref().map(|o| o.display()));
        parts.extend(h.kernel_display());
        parts.push(h.architecture.clone());
        out.push(row(style, "host", parts.join(" · ")));
        if let Some(up) = h.uptime_seconds {
            out.push(row(style, "uptime", duration(up)));
        }
        if let Some(c) = &h.container {
            out.push(row(style, "container", c));
        }
    }

    if let Some(r) = &s.resources {
        out.push(heading(style, "RESOURCES", ""));
        let mut cpu = format!("{} cores", r.cpu.logical_cores);
        if let Some(u) = r.cpu.usage_percent {
            cpu.push_str(&format!(" · {u:.0}% busy"));
        }
        if let Some(l) = r.load {
            cpu.push_str(&format!(" · load {:.2} {:.2} {:.2}", l.one, l.five, l.fifteen));
        }
        out.push(row(style, "cpu", cpu));
        out.push(row(
            style,
            "memory",
            format!(
                "{} used of {} · {} available",
                bytes(r.memory.used_bytes),
                bytes(r.memory.total_bytes),
                bytes(r.memory.available_bytes)
            ),
        ));
        if r.swap.total_bytes > 0 {
            out.push(row(style, "swap", format!("{} used of {}", bytes(r.swap.used_bytes), bytes(r.swap.total_bytes))));
        }
        for (i, d) in r.disks.iter().enumerate() {
            out.push(row(style, if i == 0 { "disks" } else { "" }, disk_line(d)));
        }
    }

    if let Some(p) = &s.processes {
        out.push(heading(style, "PROCESSES", &format!("{} total · top {} by memory", p.list.len(), TOP_PROCESSES)));
        let mut top: Vec<_> = p.list.iter().collect();
        top.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then(a.pid.cmp(&b.pid)));
        out.push(style.dim(&format!(
            "  {:>7}  {}  {}  {:>9}  {:>5}",
            "PID",
            pad("NAME", 16),
            pad("USER", 10),
            "MEMORY",
            "CPU%"
        )));
        for p in top.into_iter().take(TOP_PROCESSES) {
            out.push(process_line(p));
        }
    }

    if let Some(n) = &s.network {
        out.push(heading(style, "LISTENING", &format!("{} sockets", n.listening.len())));
        for l in &n.listening {
            out.push(format!(
                "  {}  {}  {}",
                pad(&l.protocol, 3),
                pad(&format!("{}:{}", l.address, l.port), 22),
                l.process.as_deref().unwrap_or("?")
            ));
        }
        let dns = n.dns.nameservers.join(", ");
        if !dns.is_empty() {
            out.push(row(style, "dns", dns));
        }
    }

    if let Some(d) = &s.docker {
        let engine = d.engine_version.as_ref().map(|v| format!("Docker {v} · ")).unwrap_or_default();
        out.push(heading(style, "CONTAINERS", &format!("{engine}{} containers", d.containers.len())));
        for c in &d.containers {
            out.push(container_line(c, style));
        }
    }

    if let Some(services) = &s.services {
        let failed: Vec<_> = services.iter().filter(|s| s.active_state == "failed").collect();
        let running = services.iter().filter(|s| s.sub_state == "running").count();
        out.push(heading(
            style,
            "SERVICES",
            &format!("{} units · {running} running · {} failed", services.len(), failed.len()),
        ));
        for f in failed {
            out.push(format!("  {} {}", style.red("✗"), f.name));
        }
    }

    if s.git.is_some() || s.runtimes.is_some() {
        out.push(heading(style, "APPLICATION", ""));
    }
    if let Some(g) = &s.git {
        out.push(row(style, "repo", &g.root));
        let subject = g.commit_subject.as_ref().map(|s| format!(" {}", clip(s, 60))).unwrap_or_default();
        out.push(row(
            style,
            "commit",
            format!(
                "{} @ {}{subject}",
                g.branch.as_deref().unwrap_or("(detached)"),
                g.short_commit().unwrap_or("(none)")
            ),
        ));
        let tree =
            if g.dirty { format!("dirty: {} modified, {} staged", g.modified, g.staged) } else { "clean".into() };
        out.push(row(style, "tree", tree));
    }
    if let Some(rt) = &s.runtimes {
        let list: Vec<String> =
            rt.iter().map(|r| format!("{} {}", r.name, r.version.as_deref().unwrap_or("?"))).collect();
        out.push(row(style, "runtimes", list.join(" · ")));
    }

    if let Some(e) = &s.environment {
        let redacted = e.variables.iter().filter(|v| v.redacted).count();
        out.push(heading(style, "CONFIGURATION", &format!("{} variables · {redacted} redacted", e.variables.len())));
        out.push(style.dim("  hostprint show <snapshot> --section env  lists them"));
    }
    if let Some(files) = &s.files {
        out.push(heading(style, "FILES", &format!("{} tracked", files.len())));
        for f in files {
            out.push(file_line(f));
        }
    }
    if let Some(logs) = &s.logs {
        let errors: u32 = logs.sources.iter().map(|l| l.errors).sum();
        out.push(heading(
            style,
            "LOGS",
            &format!(
                "{} since {} · {}",
                plural(logs.sources.len() as u64, "source"),
                logs.since.format("%Y-%m-%d %H:%M UTC"),
                plural(errors, "error line")
            ),
        ));
        let mut noisy: Vec<_> = logs.sources.iter().filter(|l| l.errors > 0).collect();
        noisy.sort_by(|a, b| b.errors.cmp(&a.errors).then(a.name.cmp(&b.name)));
        for src in noisy.iter().take(TOP_PROCESSES) {
            let example = src.top_errors.first().map(|p| clip(&p.example, 70)).unwrap_or_default();
            out.push(format!(
                "  {}  {}  {}",
                pad(&clip(&src.name, 28), 28),
                style.red(&pad(&plural(src.errors, "error"), 11)),
                style.dim(&example)
            ));
        }
        out.push(style.dim("  hostprint show <snapshot> --section logs  prints the lines"));
    }

    let problems: Vec<_> = s.capture.collectors.iter().filter(|c| c.status != CollectorStatus::Ok).collect();
    if !problems.is_empty() {
        out.push(heading(style, "COLLECTION", "incomplete sections"));
        for c in problems {
            let detail = c.message.clone().unwrap_or_else(|| c.notes.join("; "));
            let symbol = match c.status {
                CollectorStatus::Failed => style.red("✗"),
                CollectorStatus::Partial => style.yellow("⚠"),
                _ => style.dim("–"),
            };
            out.push(format!("  {symbol} {}  {}", pad(&c.name, 12), style.dim(&detail)));
        }
    }
    out
}

fn full_section(s: &Snapshot, section: SectionArg, style: &Style) -> Result<Vec<String>> {
    let missing = |what: &str, collector: &str| -> anyhow::Error {
        let why = s.collector(collector).and_then(|c| c.message.clone()).unwrap_or_else(|| "not collected".into());
        anyhow::anyhow!("snapshot '{}' has no {what}: {why}", s.name)
    };
    let mut out = Vec::new();
    match section {
        SectionArg::Processes => {
            let Some(p) = &s.processes else { bail!(missing("process data", "processes")) };
            out.push(style.dim(&format!(
                "{:>7}  {}  {}  {:>9}  {:>5}  COMMAND",
                "PID",
                pad("NAME", 16),
                pad("USER", 10),
                "MEMORY",
                "CPU%"
            )));
            for p in &p.list {
                out.push(format!(
                    "{}  {}",
                    process_line(p).trim_start(),
                    p.cmdline.as_deref().map(|c| clip(c, 100)).unwrap_or_default()
                ));
            }
        }
        SectionArg::Ports => {
            let Some(n) = &s.network else { bail!(missing("network data", "network")) };
            for l in &n.listening {
                let pid = l.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
                out.push(format!(
                    "{}  {}  {}  {}",
                    pad(&l.protocol, 3),
                    pad(&format!("{}:{}", l.address, l.port), 30),
                    pad(&pid, 7),
                    l.process.as_deref().unwrap_or("?")
                ));
            }
            let states: Vec<String> = n.tcp_states.iter().map(|(k, v)| format!("{k} {v}")).collect();
            out.push(style.dim(&format!("tcp sockets: {}", states.join(" · "))));
        }
        SectionArg::Interfaces => {
            let Some(n) = &s.network else { bail!(missing("network data", "network")) };
            for i in &n.interfaces {
                let kind = if i.is_virtual { " (virtual)" } else { "" };
                out.push(format!(
                    "{}  {}{}  {}",
                    pad(&i.name, 16),
                    pad(i.state.as_deref().unwrap_or("?"), 8),
                    kind,
                    i.addresses.join(", ")
                ));
            }
            for g in &n.default_gateways {
                out.push(format!("default via {} dev {}", g.gateway, g.interface));
            }
            out.push(format!("dns: {}", n.dns.nameservers.join(", ")));
        }
        SectionArg::Disks => {
            let Some(r) = &s.resources else { bail!(missing("resource data", "resources")) };
            for d in &r.disks {
                out.push(disk_line(d));
            }
        }
        SectionArg::Services => {
            let Some(services) = &s.services else { bail!(missing("service data", "services")) };
            for svc in services {
                let restarts = svc.restarts.map(|r| format!("restarts {r}")).unwrap_or_default();
                out.push(format!(
                    "{}  {}  {}",
                    pad(&svc.name, 40),
                    pad(&format!("{} ({})", svc.active_state, svc.sub_state), 24),
                    style.dim(&restarts)
                ));
            }
        }
        SectionArg::Containers => {
            let Some(d) = &s.docker else { bail!(missing("container data", "docker")) };
            for c in &d.containers {
                out.push(container_line(c, style).trim_start().to_string());
                if !c.ports.is_empty() {
                    out.push(style.dim(&format!("    ports {}", c.ports.join(", "))));
                }
            }
        }
        SectionArg::Env => {
            let Some(e) = &s.environment else { bail!(missing("environment data", "environment")) };
            for v in &e.variables {
                let source =
                    if v.source == "process" { String::new() } else { style.dim(&format!("  ({})", v.source)) };
                let value = if v.redacted { style.yellow(&v.value) } else { clip(&v.value, 120) };
                out.push(format!("{}={value}{source}", v.name));
            }
        }
        SectionArg::Runtimes => {
            let Some(rt) = &s.runtimes else { bail!(missing("runtime data", "runtimes")) };
            for r in rt {
                out.push(format!(
                    "{}  {}  {}",
                    pad(&r.name, 14),
                    pad(r.version.as_deref().unwrap_or("?"), 24),
                    style.dim(&r.path)
                ));
            }
        }
        SectionArg::Files => {
            let Some(files) = &s.files else { bail!(missing("file data", "files")) };
            for f in files {
                out.push(file_line(f).trim_start().to_string());
            }
        }
        SectionArg::Logs => {
            let Some(logs) = &s.logs else { bail!(missing("logs", "logs")) };
            for src in &logs.sources {
                out.push(format!(
                    "{} {}",
                    style.bold(&format!("{} ({})", src.name, src.kind)),
                    style.dim(&format!(
                        "{} · {} · {}{}",
                        plural(src.total, "line"),
                        plural(src.errors, "error"),
                        plural(src.warnings, "warning"),
                        if src.truncated { " · older lines not kept" } else { "" }
                    ))
                ));
                for line in &src.lines {
                    out.push(format!("  {line}"));
                }
                out.push(String::new());
            }
        }
        SectionArg::Collectors => {
            for c in &s.capture.collectors {
                let status = format!("{:?}", c.status).to_lowercase();
                let detail = c.summary.clone().or_else(|| c.message.clone()).unwrap_or_default();
                out.push(format!("{}  {}  {:>6}ms  {}", pad(&c.name, 12), pad(&status, 8), c.duration_ms, detail));
                for note in &c.notes {
                    out.push(style.dim(&format!("    {note}")));
                }
            }
        }
    }
    Ok(out)
}

fn process_line(p: &hostprint_model::Process) -> String {
    format!(
        "  {:>7}  {}  {}  {:>9}  {:>5}",
        p.pid,
        pad(&clip(&p.name, 16), 16),
        pad(&clip(p.user.as_deref().unwrap_or("?"), 10), 10),
        bytes(p.memory_bytes),
        p.cpu_percent.map(|c| format!("{c:.1}")).unwrap_or_else(|| "-".into())
    )
}

fn disk_line(d: &hostprint_model::Disk) -> String {
    if d.unresponsive {
        return format!("{}  not responding", d.mount_point);
    }
    let usage = d.usage_ratio().map(|r| format!("{:.0}%", r * 100.0)).unwrap_or_else(|| "?".into());
    let total = d.total_bytes.map(bytes).unwrap_or_else(|| "?".into());
    let free = d.available_bytes.map(bytes).unwrap_or_else(|| "?".into());
    let ro = if d.read_only { " · read-only" } else { "" };
    format!("{}  {usage} of {total} used · {free} free · {} {}{ro}", pad(&d.mount_point, 12), d.filesystem, d.device)
}

fn container_line(c: &hostprint_model::Container, style: &Style) -> String {
    // Pad before colouring so escape codes don't skew the columns.
    let health_text = pad(c.health.as_deref().unwrap_or("-"), 9);
    let health = match c.health.as_deref() {
        Some("healthy") => style.green(&health_text),
        Some("unhealthy") => style.red(&health_text),
        Some(_) => style.yellow(&health_text),
        None => style.dim(&health_text),
    };
    let state_text = pad(&c.state, 10);
    let state = if c.state == "running" { state_text } else { style.yellow(&state_text) };
    let memory = c.memory_bytes.map(bytes).unwrap_or_default();
    format!(
        "  {}  {}  {}  restarts {:<3}  {}  {}",
        pad(&clip(&c.name, 20), 20),
        state,
        health,
        c.restart_count,
        pad(&memory, 9),
        style.dim(&c.image)
    )
}

fn file_line(f: &hostprint_model::FileFingerprint) -> String {
    if !f.exists {
        return format!("  {}  missing", f.path);
    }
    let hash = f.sha256.as_deref().map(|h| &h[..12.min(h.len())]).unwrap_or("-");
    let size = f.size.map(bytes).unwrap_or_default();
    format!("  {}  {}  {}  sha256 {hash}", f.path, f.mode.as_deref().unwrap_or(""), size)
}
