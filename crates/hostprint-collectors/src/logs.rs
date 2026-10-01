//! Recent logs: the systemd journal (warning and worse), Docker container
//! output, and configured log files.
//!
//! Logs are content, not metadata, so collection is opt-in and bounded: by
//! time window, by lines kept per source, and by line length. Every line goes
//! through [`Redactor::text`](crate::redact::Redactor::text) before it is kept.
//! Error counts cover the whole window, not just the kept lines, so they stay
//! comparable between snapshots.

use crate::util::{run_command, which};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use chrono::{DateTime, Utc};
use hostprint_model::{LogPattern, LogSource, Logs};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_LINE_CHARS: usize = 500;
const MAX_SOURCES: usize = 100;
const TOP_ERRORS: usize = 5;
const MAX_PATTERN_CHARS: usize = 120;
/// Only the end of a log file is read.
const FILE_TAIL_BYTES: u64 = 256 * 1024;
/// Most recent lines requested per container (counts cover these).
const DOCKER_TAIL: usize = 2000;
const DOCKER_MAX_CONTAINERS: usize = 50;
const JOURNAL_MAX_ENTRIES: usize = 5000;

const ERROR_WORDS: &[&str] =
    &["error", "fatal", "panic", "exception", "critical", "traceback", "segfault", "out of memory", "oom-kill"];

/// What to collect, from `--logs-since` and `[logs]` in `config.toml`.
#[derive(Debug, Clone)]
pub struct LogOptions {
    pub since: DateTime<Utc>,
    pub journal: bool,
    pub docker: bool,
    pub files: Vec<PathBuf>,
    /// Lines kept per source.
    pub max_lines: usize,
}

pub struct LogCollector;

impl Collector for LogCollector {
    fn name(&self) -> &'static str {
        "logs"
    }

    fn title(&self) -> &'static str {
        "Logs"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        let Some(opts) = &ctx.logs else {
            return Err(CollectError::Unavailable(
                "not enabled; use --logs-since 30m or collect_logs = true in config.toml".into(),
            ));
        };
        let mut sources = Vec::new();
        let mut notes = Vec::new();
        let mut absent = Vec::new();

        let mut absorb = |label: &str, result: Result<(Vec<LogSource>, Vec<String>), CollectError>| match result {
            Ok((found, more_notes)) => {
                sources.extend(found);
                notes.extend(more_notes);
            }
            Err(CollectError::Unavailable(msg)) => absent.push(format!("{label}: {msg}")),
            Err(CollectError::Failed(msg)) => notes.push(format!("{label}: {msg}")),
        };
        if opts.journal {
            absorb("journal", journal(ctx, opts));
        }
        if opts.docker {
            absorb("docker", docker(ctx, opts));
        }
        for path in &opts.files {
            absorb(&path.display().to_string(), file(ctx, path, opts).map(|s| (vec![s], Vec::new())));
        }
        if sources.is_empty() && notes.is_empty() {
            let detail = if absent.is_empty() { "no sources enabled".to_string() } else { absent.join("; ") };
            return Err(CollectError::Unavailable(format!("no log sources available ({detail})")));
        }

        sources.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
        if sources.len() > MAX_SOURCES {
            notes.push(format!("{} log sources; kept the first {MAX_SOURCES}", sources.len()));
            sources.truncate(MAX_SOURCES);
        }
        let errors: u32 = sources.iter().map(|s| s.errors).sum();
        let summary = format!(
            "{} {} · {errors} error lines since {}",
            sources.len(),
            if sources.len() == 1 { "source" } else { "sources" },
            opts.since.format("%H:%M UTC")
        );
        let mut collected = Collected::new(Section::Logs(Logs { since: opts.since, sources })).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    Error,
    Warning,
    Other,
}

/// Accumulates one source's lines within the budget.
struct SourceBuilder {
    kind: &'static str,
    name: String,
    max_lines: usize,
    total: u32,
    errors: u32,
    warnings: u32,
    patterns: HashMap<String, (u32, String)>,
    lines: VecDeque<String>,
    truncated: bool,
}

impl SourceBuilder {
    fn new(kind: &'static str, name: impl Into<String>, max_lines: usize) -> Self {
        SourceBuilder {
            kind,
            name: name.into(),
            max_lines,
            total: 0,
            errors: 0,
            warnings: 0,
            patterns: HashMap::new(),
            lines: VecDeque::new(),
            truncated: false,
        }
    }

    /// Records one entry. `prefix` (timestamp, level) is shown but not part of
    /// the error pattern.
    fn push(&mut self, ctx: &CaptureContext, prefix: &str, message: &str, level: Level) {
        let message = clip(&ctx.redactor.text(message.trim_end()), MAX_LINE_CHARS);
        self.total += 1;
        match level {
            Level::Error => {
                self.errors += 1;
                let entry = self.patterns.entry(normalize(&message)).or_insert_with(|| (0, message.clone()));
                entry.0 += 1;
            }
            Level::Warning => self.warnings += 1,
            Level::Other => {}
        }
        if self.lines.len() == self.max_lines {
            self.lines.pop_front();
            self.truncated = true;
        }
        self.lines.push_back(if prefix.is_empty() { message } else { format!("{prefix} {message}") });
    }

    fn finish(self) -> LogSource {
        let mut top: Vec<LogPattern> = self
            .patterns
            .into_iter()
            .map(|(pattern, (count, example))| LogPattern { pattern, count, example })
            .collect();
        top.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.pattern.cmp(&b.pattern)));
        top.truncate(TOP_ERRORS);
        LogSource {
            kind: self.kind.to_string(),
            name: self.name,
            total: self.total,
            errors: self.errors,
            warnings: self.warnings,
            top_errors: top,
            lines: self.lines.into(),
            truncated: self.truncated,
        }
    }
}

// --- systemd journal --------------------------------------------------------

fn journal(ctx: &CaptureContext, opts: &LogOptions) -> Result<(Vec<LogSource>, Vec<String>), CollectError> {
    let journalctl = which("journalctl").ok_or_else(|| CollectError::Unavailable("journalctl not found".into()))?;
    let since = format!("--since=@{}", opts.since.timestamp());
    let limit = JOURNAL_MAX_ENTRIES.to_string();
    let fields = "--output-fields=PRIORITY,_SYSTEMD_UNIT,SYSLOG_IDENTIFIER,MESSAGE,_TRANSPORT";
    let mut args = vec!["--no-pager", "-o", "json", "-p", "warning", since.as_str(), "-n", limit.as_str(), fields];
    let mut out = run_command(&journalctl, &args, None, ctx.command_timeout)?;
    if !out.success && out.stderr.contains("output-fields") {
        // systemd before 236 has no --output-fields.
        args.pop();
        out = run_command(&journalctl, &args, None, ctx.command_timeout)?;
    }
    if out.stderr.contains("No journal files were found") {
        return Err(CollectError::Unavailable("no journal files".into()));
    }
    let restricted =
        out.stderr.contains("insufficient permissions") || out.stderr.contains("not seeing messages from other users");
    // journalctl exits non-zero when it could open no journal file at all.
    if !out.success && !restricted {
        return Err(CollectError::Failed(out.error_line()));
    }

    let mut notes = Vec::new();
    if restricted {
        notes.push(
            "journal: system messages are not readable by this user (add it to the systemd-journal or adm group, \
             or run as root)"
                .to_string(),
        );
    }
    let mut builders: HashMap<String, SourceBuilder> = HashMap::new();
    for line in out.stdout.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let Some(e) = parse_journal_entry(&entry) else { continue };
        let builder =
            builders.entry(e.source.clone()).or_insert_with(|| SourceBuilder::new("journal", e.source, opts.max_lines));
        let level = if e.priority <= 3 { Level::Error } else { Level::Warning };
        builder.push(ctx, &format!("{} {}", e.time, priority_name(e.priority)), &e.message, level);
    }
    Ok((builders.into_values().map(SourceBuilder::finish).collect(), notes))
}

pub(crate) struct JournalEntry {
    pub source: String,
    pub priority: u8,
    pub time: String,
    pub message: String,
}

pub(crate) fn parse_journal_entry(v: &Value) -> Option<JournalEntry> {
    let field = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let message = match v.get("MESSAGE")? {
        Value::String(s) => s.clone(),
        // Non-UTF-8 messages are encoded as arrays of bytes.
        Value::Array(bytes) => {
            String::from_utf8_lossy(&bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect::<Vec<_>>())
                .into_owned()
        }
        _ => return None,
    };
    let source = field("_SYSTEMD_UNIT")
        .or_else(|| field("SYSLOG_IDENTIFIER"))
        .or_else(|| (field("_TRANSPORT") == Some("kernel")).then_some("kernel"))
        .unwrap_or("journal")
        .to_string();
    let time = field("__REALTIME_TIMESTAMP")
        .and_then(|us| us.parse::<i64>().ok())
        .and_then(|us| DateTime::from_timestamp(us / 1_000_000, 0))
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let priority = field("PRIORITY").and_then(|p| p.parse().ok()).unwrap_or(6);
    Some(JournalEntry { source, priority, time, message })
}

fn priority_name(p: u8) -> &'static str {
    match p {
        0 => "emerg",
        1 => "alert",
        2 => "crit",
        3 => "err",
        4 => "warning",
        5 => "notice",
        6 => "info",
        _ => "debug",
    }
}

// --- Docker -----------------------------------------------------------------

#[cfg(unix)]
fn docker(ctx: &CaptureContext, opts: &LogOptions) -> Result<(Vec<LogSource>, Vec<String>), CollectError> {
    use crate::docker::imp::{connect_error, get_bytes, get_json, socket};
    let socket = socket()?;
    let timeout = ctx.command_timeout;
    let list = get_json(&socket, "/containers/json?all=1", timeout).map_err(|e| connect_error(&socket, e))?;
    let containers: Vec<(String, String)> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let id = c.get("Id")?.as_str()?.to_string();
                    let name = c.pointer("/Names/0")?.as_str()?.trim_start_matches('/').to_string();
                    Some((id, name))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut notes = Vec::new();
    if containers.len() > DOCKER_MAX_CONTAINERS {
        notes.push(format!("{} containers; read logs of the first {DOCKER_MAX_CONTAINERS}", containers.len()));
    }
    let containers = &containers[..containers.len().min(DOCKER_MAX_CONTAINERS)];

    let fetched: Vec<(String, Result<Vec<u8>, String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = containers
            .chunks(containers.len().div_ceil(8).max(1))
            .map(|batch| {
                let socket = &socket;
                scope.spawn(move || {
                    batch
                        .iter()
                        .map(|(id, name)| {
                            let path = format!(
                                "/containers/{id}/logs?stdout=1&stderr=1&timestamps=1&since={}&tail={DOCKER_TAIL}",
                                opts.since.timestamp()
                            );
                            (name.clone(), get_bytes(socket, &path, timeout).map_err(|e| e.to_string()))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });

    let mut sources = Vec::new();
    for (name, result) in fetched {
        let raw = match result {
            Ok(raw) => raw,
            Err(e) => {
                notes.push(format!("docker logs {name}: {e}"));
                continue;
            }
        };
        let mut builder = SourceBuilder::new("docker", name, opts.max_lines);
        for line in demux_docker_stream(&raw).lines() {
            let (time, message) = split_docker_timestamp(line);
            builder.push(ctx, &time, message, classify(message));
        }
        if builder.total > 0 {
            sources.push(builder.finish());
        }
    }
    Ok((sources, notes))
}

#[cfg(not(unix))]
fn docker(_ctx: &CaptureContext, _opts: &LogOptions) -> Result<(Vec<LogSource>, Vec<String>), CollectError> {
    Err(CollectError::Unavailable("Docker logs are only read on Unix".into()))
}

/// Docker multiplexes stdout and stderr of non-TTY containers into frames:
/// `[stream, 0, 0, 0, len (u32 BE)]` followed by `len` bytes.
pub(crate) fn demux_docker_stream(raw: &[u8]) -> String {
    let multiplexed = raw.len() >= 8 && raw[0] <= 2 && raw[1..4] == [0, 0, 0];
    if !multiplexed {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    while rest.len() >= 8 {
        let len = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let end = (8 + len).min(rest.len());
        out.extend_from_slice(&rest[8..end]);
        rest = &rest[end..];
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `2026-10-01T14:36:46.123456789Z message` → (`2026-10-01T14:36:46Z`, `message`).
fn split_docker_timestamp(line: &str) -> (String, &str) {
    match line.split_once(' ') {
        Some((ts, message)) if ts.len() >= 20 && ts.as_bytes()[4] == b'-' && ts.ends_with('Z') => {
            (format!("{}Z", &ts[..19]), message)
        }
        _ => (String::new(), line),
    }
}

// --- Files ------------------------------------------------------------------

fn file(ctx: &CaptureContext, path: &Path, opts: &LogOptions) -> Result<LogSource, CollectError> {
    let mut f = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CollectError::Unavailable("not found".into()),
        _ => CollectError::Failed(e.to_string()),
    })?;
    let len = f.metadata()?.len();
    let skipped = len > FILE_TAIL_BYTES;
    if skipped {
        f.seek(SeekFrom::Start(len - FILE_TAIL_BYTES))?;
    }
    let mut buf = Vec::new();
    f.take(FILE_TAIL_BYTES).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if skipped {
        lines.next(); // the first line is probably cut
    }
    let mut builder = SourceBuilder::new("file", path.display().to_string(), opts.max_lines);
    for line in lines.filter(|l| !l.trim().is_empty()) {
        builder.push(ctx, "", line, classify(line));
    }
    let mut source = builder.finish();
    source.truncated |= skipped;
    Ok(source)
}

// --- Helpers ----------------------------------------------------------------

pub(crate) fn classify(message: &str) -> Level {
    let lower = message.to_ascii_lowercase();
    if ERROR_WORDS.iter().any(|w| lower.contains(w)) {
        Level::Error
    } else if lower.contains("warn") {
        Level::Warning
    } else {
        Level::Other
    }
}

/// Groups messages that differ only in numbers, addresses or IDs.
pub(crate) fn normalize(message: &str) -> String {
    let words: Vec<&str> =
        message.split_whitespace().map(|w| if w.chars().any(|c| c.is_ascii_digit()) { "#" } else { w }).collect();
    clip(&words.join(" "), MAX_PATTERN_CHARS)
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Redactor;
    use serde_json::json;

    fn ctx() -> CaptureContext {
        CaptureContext::new(Redactor::new(b"k"))
    }

    #[test]
    fn parses_journal_entries() {
        let e = parse_journal_entry(&json!({
            "__REALTIME_TIMESTAMP": "1790000000123456",
            "PRIORITY": "3",
            "_SYSTEMD_UNIT": "redis-server.service",
            "MESSAGE": "Failed to open AOF"
        }))
        .unwrap();
        assert_eq!(
            (e.source.as_str(), e.priority, e.time.as_str()),
            ("redis-server.service", 3, "2026-09-21T14:13:20Z")
        );
        let kernel = parse_journal_entry(&json!({
            "_TRANSPORT": "kernel", "PRIORITY": "3", "MESSAGE": [79, 79, 77]
        }))
        .unwrap();
        assert_eq!((kernel.source.as_str(), kernel.message.as_str()), ("kernel", "OOM"));
        assert!(parse_journal_entry(&json!({"PRIORITY": "3"})).is_none());
    }

    #[test]
    fn demultiplexes_docker_streams() {
        let mut raw = vec![1, 0, 0, 0, 0, 0, 0, 6];
        raw.extend_from_slice(b"hello\n");
        raw.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 4]);
        raw.extend_from_slice(b"err\n");
        assert_eq!(demux_docker_stream(&raw), "hello\nerr\n");
        assert_eq!(demux_docker_stream(b"plain tty output\n"), "plain tty output\n");
        assert_eq!(
            split_docker_timestamp("2026-10-01T14:36:46.123456789Z FATAL: boom"),
            ("2026-10-01T14:36:46Z".to_string(), "FATAL: boom")
        );
    }

    #[test]
    fn counts_errors_keeps_recent_lines_and_redacts() {
        let ctx = ctx();
        let mut b = SourceBuilder::new("docker", "redis", 2);
        b.push(&ctx, "t1", "Ready to accept connections", classify("Ready to accept connections"));
        b.push(&ctx, "t2", "WARNING overcommit_memory is set to 0", Level::Warning);
        b.push(&ctx, "t3", "FATAL: cannot open file 17 (password=hunter2)", Level::Error);
        b.push(&ctx, "t4", "FATAL: cannot open file 18 (password=hunter2)", Level::Error);
        let s = b.finish();
        assert_eq!((s.total, s.errors, s.warnings), (4, 2, 1));
        assert!(s.truncated);
        assert_eq!(
            s.lines,
            [
                "t3 FATAL: cannot open file 17 (password=[REDACTED])",
                "t4 FATAL: cannot open file 18 (password=[REDACTED])"
            ]
        );
        assert_eq!(s.top_errors.len(), 1, "numbers don't split patterns");
        assert_eq!(s.top_errors[0].count, 2);
        assert_eq!(s.top_errors[0].pattern, "FATAL: cannot open file # (password=[REDACTED])");
    }

    #[test]
    fn classifies_lines() {
        assert_eq!(classify("level=error msg=\"dial tcp: refused\""), Level::Error);
        assert_eq!(classify("Traceback (most recent call last):"), Level::Error);
        assert_eq!(classify("WARN slow query"), Level::Warning);
        assert_eq!(classify("GET /health 200"), Level::Other);
    }

    #[test]
    fn reads_the_tail_of_log_files() {
        let dir = std::env::temp_dir().join(format!("hostprint-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        std::fs::write(&path, "start\nERROR db timeout token=abc\nok\n").unwrap();
        let opts =
            LogOptions { since: Utc::now(), journal: false, docker: false, files: vec![path.clone()], max_lines: 10 };
        let s = file(&ctx(), &path, &opts).unwrap();
        assert_eq!((s.total, s.errors), (3, 1));
        assert_eq!(s.lines[1], "ERROR db timeout token=[REDACTED]");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
