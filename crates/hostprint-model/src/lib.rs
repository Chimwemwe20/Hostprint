//! Snapshot data model for Hostprint.
//!
//! A [`Snapshot`] is the state of one machine at one point in time. Its JSON
//! encoding *is* the on-disk format, so every change here is a format change:
//! additive, optional fields are fine within a schema version; anything else
//! must bump [`SCHEMA_VERSION`] and be described in `docs/snapshot-format.md`.
//!
//! Every top-level section is optional. `None` means "not collected" (the
//! collector was skipped or failed — see [`CaptureInfo::collectors`]), which is
//! different from "collected and empty". The diff engine relies on that
//! distinction to avoid reporting every container as removed just because the
//! Docker daemon could not be reached.

pub mod format;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version of the snapshot format produced by this crate.
pub const SCHEMA_VERSION: u32 = 1;

/// Placeholder stored in place of a secret value.
pub const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub captured_at: DateTime<Utc>,
    pub capture: CaptureInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Host>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<Processes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<Service>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<Docker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<Git>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<Vec<Runtime>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<FileFingerprint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<Logs>,
}

impl Snapshot {
    /// The report for a collector, by its stable name (e.g. `"docker"`).
    pub fn collector(&self, name: &str) -> Option<&CollectorReport> {
        self.capture.collectors.iter().find(|c| c.name == name)
    }

    pub fn hostname(&self) -> Option<&str> {
        self.host.as_ref().map(|h| h.hostname.as_str())
    }
}

/// How the snapshot was taken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInfo {
    pub hostprint_version: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Whether the capture ran as root. Non-root captures can be missing
    /// process and socket details owned by other users.
    pub elevated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    pub collectors: Vec<CollectorReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorReport {
    pub name: String,
    pub status: CollectorStatus,
    pub duration_ms: u64,
    /// One-line description of what was collected, e.g. "412 processes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Why the collector was skipped or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Caveats for partial results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectorStatus {
    /// Collected completely.
    Ok,
    /// Collected, with caveats in `notes` (usually missing permissions).
    Partial,
    /// Not applicable here: tool not installed, not a Git repository, etc.
    Skipped,
    /// Applicable but could not be collected; see `message`.
    Failed,
}

impl CollectorStatus {
    pub fn has_data(self) -> bool {
        matches!(self, CollectorStatus::Ok | CollectorStatus::Partial)
    }
}

// ---------------------------------------------------------------------------
// System

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OsRelease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    pub architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// DMI vendor and product, e.g. "QEMU Standard PC (Q35 + ICH9, 2009)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<String>,
    /// Container runtime Hostprint itself is running inside, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsRelease {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pretty_name: Option<String>,
}

impl OsRelease {
    pub fn display(&self) -> String {
        if let Some(pretty) = &self.pretty_name {
            return pretty.clone();
        }
        match (&self.name, &self.version_id) {
            (Some(n), Some(v)) => format!("{n} {v}"),
            (Some(n), None) => n.clone(),
            _ => self.id.clone().unwrap_or_else(|| "unknown".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Resources

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    pub cpu: Cpu,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<LoadAverage>,
    pub memory: Memory,
    pub swap: Swap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Pressure>,
    pub disks: Vec<Disk>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cpu {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub logical_cores: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_cores: Option<u32>,
    /// Busy percentage over a short sampling window during capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iowait_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steal_percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total_bytes: u64,
    /// Memory available for new workloads without swapping (`MemAvailable`).
    pub available_bytes: u64,
    /// `total - available`.
    pub used_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Swap {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
}

/// Linux pressure stall information: share of the last 60 seconds in which
/// tasks were stalled on a resource, as a percentage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pressure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_some_avg60: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_some_avg60: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_full_avg60: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_some_avg60: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_full_avg60: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub mount_point: String,
    pub device: String,
    pub filesystem: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// The filesystem did not answer `statvfs` in time (e.g. a hung NFS mount).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unresponsive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inodes_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inodes_free: Option<u64>,
}

impl Disk {
    /// Used fraction as `df` reports it: used / (used + available), which
    /// excludes blocks reserved for root.
    pub fn usage_ratio(&self) -> Option<f64> {
        let used = self.used_bytes? as f64;
        let avail = self.available_bytes? as f64;
        (used + avail > 0.0).then(|| used / (used + avail))
    }

    pub fn inode_usage_ratio(&self) -> Option<f64> {
        let total = self.inodes_total?;
        let free = self.inodes_free?;
        (total > 0).then(|| (total.saturating_sub(free)) as f64 / total as f64)
    }
}

// ---------------------------------------------------------------------------
// Processes

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Processes {
    /// User-space processes, excluding Hostprint itself and its children.
    pub list: Vec<Process>,
    /// Kernel threads are counted but not listed; they churn constantly.
    pub kernel_threads: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// Command line with secret-looking arguments redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Single-letter kernel state: R, S, D, Z, T, I, ...
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    pub memory_bytes: u64,
    pub threads: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Network

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub interfaces: Vec<Interface>,
    pub listening: Vec<ListeningSocket>,
    /// Count of non-listening TCP sockets by state (ESTABLISHED, TIME_WAIT, ...).
    pub tcp_states: BTreeMap<String, u64>,
    pub default_gateways: Vec<Route>,
    pub dns: Dns,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_ports: Option<PortRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interface {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    /// Addresses in CIDR notation.
    pub addresses: Vec<String>,
    /// Software interface (bridge, veth, tun, loopback, ...).
    #[serde(rename = "virtual", default, skip_serializing_if = "is_false")]
    pub is_virtual: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningSocket {
    /// "tcp" or "udp".
    pub protocol: String,
    pub address: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    pub gateway: String,
    pub interface: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dns {
    pub nameservers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search: Vec<String>,
    /// Upstream servers behind a local stub resolver such as systemd-resolved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream_nameservers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl PortRange {
    pub fn contains(&self, port: u16) -> bool {
        (self.start..=self.end).contains(&port)
    }
}

// ---------------------------------------------------------------------------
// Services

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    /// systemd service type: simple, forking, oneshot, notify, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_type: Option<String>,
    /// Automatic restarts performed by systemd (`NRestarts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restarts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_pid: Option<u32>,
}

// ---------------------------------------------------------------------------
// Docker

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Docker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    pub containers: Vec<Container>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    /// Short (12 character) container ID.
    pub id: String,
    pub name: String,
    pub image: String,
    /// Short local image ID; changes when the tag is rebuilt or re-pulled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    /// created, running, restarting, paused, exited, dead, removing.
    pub state: String,
    /// Docker's human status, e.g. "Up 3 hours (healthy)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// healthy, unhealthy, starting; absent without a health check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    pub restart_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub oom_killed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    pub ports: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_service: Option<String>,
}

// ---------------------------------------------------------------------------
// Application

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Git {
    pub root: String,
    /// `None` when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Full commit SHA; `None` in a repository without commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub describe: Option<String>,
    /// `origin` URL with any credentials redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub dirty: bool,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    /// Paths of tracked files with changes (capped; never file contents).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
}

impl Git {
    pub fn short_commit(&self) -> Option<&str> {
        self.commit.as_deref().map(|c| &c[..c.len().min(7)])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Configuration

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    /// Identifies the key used for secret fingerprints. Fingerprints are only
    /// comparable between snapshots with the same key ID.
    pub fingerprint_key_id: String,
    pub variables: Vec<EnvVar>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    /// "process" for Hostprint's own environment, otherwise the env file path.
    pub source: String,
    /// The value, or [`REDACTED`] (possibly partially, e.g. a URL with its
    /// password removed).
    pub value: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted: bool,
    /// Keyed hash of the original value, present when the value is not stored
    /// verbatim. Lets the diff detect a changed secret without revealing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFingerprint {
    pub path: String,
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Permission bits in octal, e.g. "0644".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Logs

/// Recent log lines, bounded by time window, line count and line length.
/// Lines are redacted like every other value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Logs {
    /// Start of the collection window.
    pub since: DateTime<Utc>,
    pub sources: Vec<LogSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSource {
    /// "journal", "docker" or "file".
    pub kind: String,
    /// systemd unit or syslog identifier, container name, or file path.
    pub name: String,
    /// Lines in the window, including ones not kept in `lines`.
    pub total: u32,
    /// Lines that look like errors (journal priority err or worse, or an
    /// error keyword in other sources).
    pub errors: u32,
    pub warnings: u32,
    /// The most frequent error messages, normalised so that numbers and IDs
    /// don't make every occurrence unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_errors: Vec<LogPattern>,
    /// The most recent lines, oldest first.
    pub lines: Vec<String>,
    /// More lines existed than were kept.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPattern {
    /// Message with digits and identifiers replaced, e.g. "connection to # refused".
    pub pattern: String,
    pub count: u32,
    /// One original (redacted) occurrence.
    pub example: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}
