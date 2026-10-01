//! Collectors gather one area of system state each.
//!
//! Collectors are independent: the capture engine runs them in parallel, and
//! one failing (Docker daemon down, no permission) never fails the others.
//! Each reports either data plus optional caveats, or a [`CollectError`] that
//! says whether it was not applicable here or genuinely failed.
//!
//! Linux collectors read `/proc`, `/sys` and `/etc` through
//! [`CaptureContext::path`], so tests can point them at a fixture tree.

pub mod docker;
pub mod environment;
pub mod files;
pub mod git;
pub mod logs;
pub mod network;
pub mod processes;
pub mod redact;
pub mod resources;
pub mod runtimes;
pub mod services;
pub mod system;
mod util;

use hostprint_model as model;
use redact::Redactor;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use util::{run_command, which, CommandOutput};

/// Everything a collector needs to know about the capture in progress.
#[derive(Debug, Clone)]
pub struct CaptureContext {
    /// Filesystem root that `/proc`, `/sys` and `/etc` are read under.
    /// Always `/` outside of tests.
    pub root: PathBuf,
    /// Directory the Git collector inspects.
    pub repo_dir: PathBuf,
    pub redactor: Redactor,
    /// Whether to record Hostprint's own process environment.
    pub capture_process_env: bool,
    /// Dotenv-style files whose variables are recorded (redacted).
    pub env_files: Vec<PathBuf>,
    /// Files to fingerprint.
    pub file_paths: Vec<PathBuf>,
    /// Log collection settings; `None` leaves logs out (the default).
    pub logs: Option<logs::LogOptions>,
    /// Process whose subtree is excluded from the process list (Hostprint
    /// itself, so its helper commands don't show up as changes).
    pub self_pid: Option<u32>,
    /// Upper bound for any external command a collector runs.
    pub command_timeout: Duration,
    /// Upper bound for a whole collector; the capture moves on without it.
    pub collector_timeout: Duration,
    /// Window over which CPU usage is sampled.
    pub sample_interval: Duration,
}

impl CaptureContext {
    pub fn new(redactor: Redactor) -> Self {
        CaptureContext {
            root: PathBuf::from("/"),
            repo_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            redactor,
            capture_process_env: true,
            env_files: Vec::new(),
            file_paths: Vec::new(),
            logs: None,
            self_pid: Some(std::process::id()),
            command_timeout: Duration::from_secs(5),
            collector_timeout: Duration::from_secs(20),
            sample_interval: Duration::from_millis(250),
        }
    }

    /// Resolves an absolute system path such as `/proc/meminfo` under [`root`](Self::root).
    pub fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    /// Whether the context reads the live system rather than a fixture tree.
    pub fn is_live(&self) -> bool {
        self.root == Path::new("/")
    }

    /// Linux collectors call this first. Fixture trees are allowed anywhere.
    pub(crate) fn require_linux(&self) -> Result<(), CollectError> {
        if cfg!(target_os = "linux") || !self.is_live() {
            Ok(())
        } else {
            Err(CollectError::Unavailable(format!("not supported on {} yet", std::env::consts::OS)))
        }
    }
}

/// One section of a snapshot, as produced by a collector.
#[derive(Debug, Clone)]
pub enum Section {
    Host(model::Host),
    Resources(model::Resources),
    Processes(model::Processes),
    Network(model::Network),
    Services(Vec<model::Service>),
    Docker(model::Docker),
    Git(model::Git),
    Runtimes(Vec<model::Runtime>),
    Environment(model::Environment),
    Files(Vec<model::FileFingerprint>),
    Logs(model::Logs),
}

/// A successful collection.
#[derive(Debug, Clone)]
pub struct Collected {
    pub section: Section,
    /// One-line description, e.g. "412 processes".
    pub summary: Option<String>,
    /// Caveats that make the result partial, e.g. missing permissions.
    pub notes: Vec<String>,
}

impl Collected {
    pub fn new(section: Section) -> Self {
        Collected { section, summary: None, notes: Vec::new() }
    }

    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectError {
    /// Not applicable on this machine (tool missing, not a Git repo, ...).
    Unavailable(String),
    /// Applicable but could not be collected.
    Failed(String),
}

impl fmt::Display for CollectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CollectError::Unavailable(msg) | CollectError::Failed(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CollectError {}

impl From<std::io::Error> for CollectError {
    fn from(err: std::io::Error) -> Self {
        CollectError::Failed(err.to_string())
    }
}

pub trait Collector: Send + Sync {
    /// Stable identifier used in snapshots, e.g. `"docker"`.
    fn name(&self) -> &'static str;
    /// Display name, e.g. `"Docker"`.
    fn title(&self) -> &'static str;
    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError>;
}

/// Effective UID and user name of this process.
pub fn current_user() -> (Option<u32>, Option<String>) {
    let uid = util::euid();
    let name = uid
        .and_then(|uid| {
            let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
            util::parse_passwd(&passwd).remove(&uid)
        })
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok());
    (uid, name)
}

/// The collectors that make up a standard capture, in display order.
pub fn default_collectors() -> Vec<Arc<dyn Collector>> {
    vec![
        Arc::new(system::SystemCollector),
        Arc::new(resources::ResourcesCollector),
        Arc::new(processes::ProcessCollector),
        Arc::new(network::NetworkCollector),
        Arc::new(services::ServiceCollector),
        Arc::new(docker::DockerCollector),
        Arc::new(git::GitCollector),
        Arc::new(runtimes::RuntimeCollector),
        Arc::new(environment::EnvironmentCollector),
        Arc::new(files::FileCollector),
        Arc::new(logs::LogCollector),
    ]
}

/// Applies `--only` / `--skip` / `[collectors] disable`. Collectors that are
/// turned off still appear in the capture report, as skipped with "disabled",
/// so a diff can tell "turned off" from "failed".
pub fn select(
    collectors: Vec<Arc<dyn Collector>>,
    only: &[String],
    skip: &[String],
) -> Result<Vec<Arc<dyn Collector>>, String> {
    let known: Vec<&str> = collectors.iter().map(|c| c.name()).collect();
    if let Some(unknown) = only.iter().chain(skip).find(|n| !known.contains(&n.as_str())) {
        return Err(format!("unknown collector '{unknown}' (known: {})", known.join(", ")));
    }
    Ok(collectors
        .into_iter()
        .map(|c| {
            let on = (only.is_empty() || only.iter().any(|n| n == c.name())) && !skip.iter().any(|n| n == c.name());
            if on {
                c
            } else {
                Arc::new(Disabled { name: c.name(), title: c.title() }) as Arc<dyn Collector>
            }
        })
        .collect())
}

/// Stand-in for a collector turned off by configuration.
struct Disabled {
    name: &'static str,
    title: &'static str,
}

impl Collector for Disabled {
    fn name(&self) -> &'static str {
        self.name
    }

    fn title(&self) -> &'static str {
        self.title
    }

    fn collect(&self, _ctx: &CaptureContext) -> Result<Collected, CollectError> {
        Err(CollectError::Unavailable("disabled".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(&'static str);
    impl Collector for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        fn title(&self) -> &'static str {
            self.0
        }
        fn collect(&self, _: &CaptureContext) -> Result<Collected, CollectError> {
            Err(CollectError::Failed("ran".into()))
        }
    }

    #[test]
    fn selects_collectors() {
        let all = || -> Vec<Arc<dyn Collector>> {
            vec![Arc::new(Fake("system")), Arc::new(Fake("git")), Arc::new(Fake("logs"))]
        };
        let ctx = CaptureContext::new(Redactor::new(b"k"));
        let enabled = |v: &[Arc<dyn Collector>]| -> Vec<&'static str> {
            v.iter().filter(|c| matches!(c.collect(&ctx), Err(CollectError::Failed(_)))).map(|c| c.name()).collect()
        };
        let only = select(all(), &["system".into(), "logs".into()], &[]).unwrap();
        assert_eq!(only.len(), 3, "disabled collectors still report");
        assert_eq!(enabled(&only), ["system", "logs"]);
        assert_eq!(only[1].collect(&ctx).unwrap_err(), CollectError::Unavailable("disabled".into()));
        assert_eq!(enabled(&select(all(), &[], &["git".into()]).unwrap()), ["system", "logs"]);
        assert!(matches!(select(all(), &["bogus".into()], &[]), Err(e) if e.contains("unknown collector")));
        assert_eq!(default_collectors().len(), 11);
    }
}
