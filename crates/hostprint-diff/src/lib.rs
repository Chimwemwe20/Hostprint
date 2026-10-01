//! Compares two snapshots and classifies what changed.
//!
//! The engine is deterministic and rule-based: every [`Change`] carries the
//! `rule` that produced it, and the same pair of snapshots always yields the
//! same changes in the same order. It reports evidence ("restart count 0 →
//! 17"), never conclusions ("Redis caused the outage").
//!
//! Noise is handled by the rules themselves: PIDs, uptime and timestamps are
//! never compared directly, fluctuating metrics only register past thresholds,
//! and churn-prone things (ephemeral ports, virtual interfaces, short-lived
//! processes, terminal variables) are demoted to `info`. The full rule list
//! is in `docs/diff-rules.md`.

mod application;
mod configuration;
mod containers;
mod files;
mod network;
mod processes;
mod resources;
mod services;
mod system;

use chrono::{DateTime, Utc};
use hostprint_model::{CollectorStatus, Snapshot};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Version of the diff JSON format.
pub const DIFF_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Significance {
    Info,
    Low,
    Medium,
    High,
}

impl Significance {
    pub fn label(self) -> &'static str {
        match self {
            Significance::High => "HIGH",
            Significance::Medium => "MEDIUM",
            Significance::Low => "LOW",
            Significance::Info => "INFO",
        }
    }

    pub fn parse(s: &str) -> Option<Significance> {
        match s.to_ascii_lowercase().as_str() {
            "high" => Some(Significance::High),
            "medium" => Some(Significance::Medium),
            "low" => Some(Significance::Low),
            "info" => Some(Significance::Info),
            _ => None,
        }
    }
}

impl fmt::Display for Significance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Comparison categories, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    System,
    Resources,
    Processes,
    Network,
    Services,
    Containers,
    Application,
    Configuration,
    Files,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::System => "SYSTEM",
            Category::Resources => "RESOURCES",
            Category::Processes => "PROCESSES",
            Category::Network => "NETWORK",
            Category::Services => "SERVICES",
            Category::Containers => "CONTAINERS",
            Category::Application => "APPLICATION",
            Category::Configuration => "CONFIGURATION",
            Category::Files => "FILES",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

/// One observed difference between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub significance: Significance,
    pub category: Category,
    pub kind: ChangeKind,
    /// Stable identifier of the rule that produced this change, e.g. `container.health`.
    pub rule: String,
    /// Stable identifier of the thing that changed, e.g. `containers/redis/health`.
    pub key: String,
    /// What changed, e.g. a container, service or variable name.
    pub subject: String,
    /// Which aspect of the subject changed, e.g. "health".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Size of the change, e.g. "+17" or "-82%".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

impl Change {
    pub(crate) fn new(
        significance: Significance,
        category: Category,
        rule: &str,
        key: impl Into<String>,
        subject: impl Into<String>,
    ) -> Change {
        Change {
            significance,
            category,
            kind: ChangeKind::Changed,
            rule: rule.to_string(),
            key: key.into(),
            subject: subject.into(),
            field: None,
            before: None,
            after: None,
            delta: None,
        }
    }

    pub(crate) fn field(mut self, field: impl Into<String>) -> Change {
        self.field = Some(field.into());
        self
    }

    pub(crate) fn values(mut self, before: impl Into<String>, after: impl Into<String>) -> Change {
        self.before = Some(before.into());
        self.after = Some(after.into());
        self
    }

    pub(crate) fn added(mut self, after: impl Into<String>) -> Change {
        self.kind = ChangeKind::Added;
        self.after = Some(after.into());
        self
    }

    pub(crate) fn removed(mut self, before: impl Into<String>) -> Change {
        self.kind = ChangeKind::Removed;
        self.before = Some(before.into());
        self
    }

    pub(crate) fn delta(mut self, delta: impl Into<String>) -> Change {
        self.delta = Some(delta.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRef {
    pub id: String,
    pub name: String,
    pub captured_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

impl SnapshotRef {
    fn of(s: &Snapshot) -> SnapshotRef {
        SnapshotRef {
            id: s.id.clone(),
            name: s.name.clone(),
            captured_at: s.captured_at,
            hostname: s.hostname().map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub info: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub schema_version: u32,
    pub from: SnapshotRef,
    pub to: SnapshotRef,
    pub summary: Summary,
    /// Caveats about the comparison itself (different hosts, missing data).
    pub notes: Vec<String>,
    /// Most significant first.
    pub changes: Vec<Change>,
}

impl Diff {
    pub fn highest(&self) -> Option<Significance> {
        self.changes.iter().map(|c| c.significance).max()
    }

    pub fn at_least(&self, min: Significance) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(move |c| c.significance >= min)
    }
}

/// Things to leave out of the comparison, from `[ignore]` in `config.toml`.
/// Name lists accept `*` wildcards.
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    pub ignore_processes: Vec<String>,
    pub ignore_ports: Vec<u16>,
    pub ignore_env: Vec<String>,
    pub ignore_containers: Vec<String>,
    pub ignore_services: Vec<String>,
}

pub fn diff(from: &Snapshot, to: &Snapshot, opts: &DiffOptions) -> Diff {
    let mut changes = Vec::new();
    let mut notes = Vec::new();

    if let (Some(a), Some(b)) = (from.hostname(), to.hostname()) {
        if a != b {
            notes.push(format!("Comparing different hosts: {a} → {b}."));
        }
    }
    if from.capture.elevated != to.capture.elevated {
        let (root, user) = if from.capture.elevated { (&from.name, &to.name) } else { (&to.name, &from.name) };
        notes.push(format!(
            "'{root}' was captured as root and '{user}' was not; process and socket ownership is compared only where both have it, and environment differences may come from sudo."
        ));
    }
    if to.captured_at < from.captured_at {
        notes.push(format!("'{}' was captured before '{}'.", to.name, from.name));
    }

    let mut ctx = Context { from, to, changes: &mut changes, notes: &mut notes };
    if let Some((a, b)) = ctx.pair("system", Category::System, |s| s.host.as_ref()) {
        system::compare(a, b, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("resources", Category::Resources, |s| s.resources.as_ref()) {
        resources::compare(a, b, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("processes", Category::Processes, |s| s.processes.as_ref()) {
        processes::compare(a, b, from.captured_at, to.captured_at, opts, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("network", Category::Network, |s| s.network.as_ref()) {
        network::compare(a, b, opts, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("services", Category::Services, |s| s.services.as_ref()) {
        services::compare(a, b, opts, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("docker", Category::Containers, |s| s.docker.as_ref()) {
        containers::compare(a, b, opts, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("git", Category::Application, |s| s.git.as_ref()) {
        application::compare_git(a, b, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("runtimes", Category::Application, |s| s.runtimes.as_ref()) {
        application::compare_runtimes(a, b, ctx.changes);
    }
    if let Some((a, b)) = ctx.pair("environment", Category::Configuration, |s| s.environment.as_ref()) {
        configuration::compare(a, b, opts, ctx.changes, ctx.notes);
    }
    if let Some((a, b)) = ctx.pair("files", Category::Files, |s| s.files.as_ref()) {
        files::compare(a, b, ctx.changes);
    }

    changes.sort_by(|a, b| {
        b.significance
            .cmp(&a.significance)
            .then(a.category.cmp(&b.category))
            .then_with(|| a.subject.cmp(&b.subject))
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut summary = Summary::default();
    for c in &changes {
        match c.significance {
            Significance::High => summary.high += 1,
            Significance::Medium => summary.medium += 1,
            Significance::Low => summary.low += 1,
            Significance::Info => summary.info += 1,
        }
    }
    Diff {
        schema_version: DIFF_SCHEMA_VERSION,
        from: SnapshotRef::of(from),
        to: SnapshotRef::of(to),
        summary,
        notes,
        changes,
    }
}

struct Context<'a> {
    from: &'a Snapshot,
    to: &'a Snapshot,
    changes: &'a mut Vec<Change>,
    notes: &'a mut Vec<String>,
}

impl<'a> Context<'a> {
    /// Both snapshots' copies of a section, or `None` after recording why
    /// the section cannot be compared.
    fn pair<T: ?Sized>(
        &mut self,
        collector: &str,
        category: Category,
        get: impl Fn(&'a Snapshot) -> Option<&'a T>,
    ) -> Option<(&'a T, &'a T)> {
        match (get(self.from), get(self.to)) {
            (Some(a), Some(b)) => Some((a, b)),
            (None, None) => None,
            (Some(_), None) => {
                self.unavailable(collector, category, self.to, true);
                None
            }
            (None, Some(_)) => {
                self.unavailable(collector, category, self.from, false);
                None
            }
        }
    }

    fn unavailable(&mut self, collector: &str, category: Category, missing_in: &Snapshot, lost: bool) {
        let title = collector_title(collector);
        let report = missing_in.collector(collector);
        let reason = report.and_then(|r| r.message.clone()).unwrap_or_else(|| "not collected".to_string());
        self.notes.push(format!("{title} not compared: no data in '{}' ({reason}).", missing_in.name));
        // Losing a data source that worked before is itself evidence: a
        // daemon that stopped answering, a socket that went away.
        if lost {
            if let Some(r) = report.filter(|r| r.status == CollectorStatus::Failed) {
                self.changes.push(
                    Change::new(
                        Significance::Medium,
                        category,
                        "collector.failed",
                        format!("collectors/{collector}"),
                        title,
                    )
                    .field("collection")
                    .values("ok", r.message.clone().unwrap_or_else(|| "failed".into())),
                );
            }
        }
    }
}

fn collector_title(name: &str) -> &'static str {
    match name {
        "system" => "System",
        "resources" => "Resources",
        "processes" => "Processes",
        "network" => "Network",
        "services" => "Services",
        "docker" => "Docker",
        "git" => "Git",
        "runtimes" => "Runtimes",
        "environment" => "Environment",
        "files" => "Files",
        _ => "Collector",
    }
}

/// Matches `name` against patterns that may contain `*` wildcards.
pub(crate) fn matches_any(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| glob(p, name))
}

fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

pub(crate) fn opt_or<'a>(value: Option<&'a str>, missing: &'a str) -> &'a str {
    value.unwrap_or(missing)
}

pub(crate) fn time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// "+a −b" summary of set differences, listing at most a few items.
pub(crate) fn set_delta<'a>(
    added: impl IntoIterator<Item = &'a str>,
    removed: impl IntoIterator<Item = &'a str>,
) -> String {
    const MAX: usize = 4;
    let mut parts: Vec<String> = Vec::new();
    let mut push = |prefix: &str, items: Vec<&str>| {
        for item in items.iter().take(MAX) {
            parts.push(format!("{prefix}{item}"));
        }
        if items.len() > MAX {
            parts.push(format!("{prefix}{} more", items.len() - MAX));
        }
    };
    push("+", added.into_iter().collect());
    push("−", removed.into_iter().collect());
    parts.join(" ")
}

pub(crate) fn major_version(v: &str) -> &str {
    v.split(['.', '-', '+']).next().unwrap_or(v)
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;
    use testing::*;

    #[test]
    fn identical_snapshots_have_no_changes() {
        let a = baseline();
        let d = diff(&a, &a.clone(), &DiffOptions::default());
        assert_eq!(d.changes, [], "{:#?}", d.changes);
        assert!(d.notes.is_empty());
    }

    #[test]
    fn changes_are_sorted_by_significance_then_category() {
        let a = baseline();
        let mut b = a.clone();
        b.git.as_mut().unwrap().commit = Some("e821aa4000000000000000000000000000000000".into());
        b.docker.as_mut().unwrap().containers[0].health = Some("unhealthy".into());
        b.runtimes.as_mut().unwrap()[0].version = Some("22.9.1".into());
        let d = diff(&a, &b, &DiffOptions::default());
        let order: Vec<(Significance, &str)> = d.changes.iter().map(|c| (c.significance, c.rule.as_str())).collect();
        assert_eq!(
            order,
            [
                (Significance::High, "container.health"),
                (Significance::Medium, "git.commit"),
                (Significance::Low, "runtime.version"),
            ]
        );
        assert_eq!(d.summary, Summary { high: 1, medium: 1, low: 1, info: 0 });
        assert_eq!(d.highest(), Some(Significance::High));
    }

    #[test]
    fn a_collector_that_stops_working_is_evidence_not_mass_removal() {
        let a = baseline();
        let mut b = a.clone();
        b.docker = None;
        set_status(&mut b, "docker", CollectorStatus::Failed, "Docker daemon not reachable at /var/run/docker.sock");
        let d = diff(&a, &b, &DiffOptions::default());
        assert_eq!(d.changes.len(), 1, "{:#?}", d.changes);
        assert_eq!(d.changes[0].rule, "collector.failed");
        assert_eq!(d.changes[0].significance, Significance::Medium);
        assert!(d.notes[0].starts_with("Docker not compared"));

        // A skipped collector (e.g. not a Git repository) is only a note.
        let mut c = a.clone();
        c.git = None;
        set_status(&mut c, "git", CollectorStatus::Skipped, "not inside a Git repository");
        let d = diff(&a, &c, &DiffOptions::default());
        assert!(d.changes.is_empty());
        assert_eq!(d.notes.len(), 1);
    }

    #[test]
    fn notes_cross_host_and_privilege_differences() {
        let a = baseline();
        let mut b = a.clone();
        b.host.as_mut().unwrap().hostname = "web-2".into();
        b.capture.elevated = true;
        let d = diff(&a, &b, &DiffOptions::default());
        assert!(d.notes.iter().any(|n| n.contains("different hosts")));
        assert!(d.notes.iter().any(|n| n.contains("as root")));
    }

    #[test]
    fn globs() {
        assert!(glob("chrome", "chrome"));
        assert!(!glob("chrome", "chromium"));
        assert!(glob("chrom*", "chromium"));
        assert!(glob("*worker", "sidekiq-worker"));
        assert!(glob("kube*proxy", "kube-proxy"));
        assert!(glob("*", "anything"));
        assert!(!glob("a*b*c", "acb"));
    }

    #[test]
    fn diff_json_round_trips() {
        let a = baseline();
        let mut b = a.clone();
        b.resources.as_mut().unwrap().memory.available_bytes = 700 * MIB;
        let d = diff(&a, &b, &DiffOptions::default());
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"significance\":\"high\""), "{json}");
        let back: Diff = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
