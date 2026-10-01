//! Diff policies: site-specific adjustments to the built-in rules.
//!
//! A policy can change the level of the changes a rule produces, or turn
//! them off, optionally only for some subjects; and it can move the main
//! numeric thresholds. It never invents changes. Every adjustment is recorded
//! on the change it affected ([`PolicyNote`]), and changes turned off are
//! counted in the diff's notes, so a policy is always visible in the output.

use crate::{glob, Change, Significance};
use serde::{Deserialize, Serialize};

/// What a policy rule does to the changes it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Leave the change out of the diff.
    Off,
    /// Report the change at this level instead.
    Level(Significance),
}

impl Action {
    pub fn parse(s: &str) -> Option<Action> {
        match s.to_ascii_lowercase().as_str() {
            "off" | "ignore" => Some(Action::Off),
            other => Significance::parse(other).map(Action::Level),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Off => "off",
            Action::Level(s) => s.label(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyRule {
    /// Glob over rule ids, e.g. `container.*`.
    pub rule: String,
    /// Glob over the change's subject, e.g. `payments-*`.
    pub subject: Option<String>,
    pub action: Action,
    /// Where the rule was defined, e.g. `config.toml`.
    pub source: String,
}

impl PolicyRule {
    pub fn matches(&self, change: &Change) -> bool {
        glob(&self.rule, &change.rule) && self.subject.as_deref().is_none_or(|s| glob(s, &change.subject))
    }

    pub fn describe(&self) -> String {
        match &self.subject {
            Some(subject) => format!("{} for {} ({})", self.rule, subject, self.source),
            None => format!("{} ({})", self.rule, self.source),
        }
    }
}

/// The numeric limits rules use, as fractions (0.95 = 95%).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Disk or inode usage at or above this is HIGH.
    pub disk_high: f64,
    /// At or above this is MEDIUM.
    pub disk_medium: f64,
    /// Available memory dropping below this share of total is HIGH.
    pub memory_available_high: f64,
    /// 1-minute load per core at or above this is HIGH.
    pub load_high: f64,
    /// At or above this is MEDIUM.
    pub load_medium: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { disk_high: 0.95, disk_medium: 0.90, memory_available_high: 0.10, load_high: 2.0, load_medium: 1.0 }
    }
}

impl Thresholds {
    /// Problems with the values, as messages.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, v) in [
            ("disk_high_percent", self.disk_high),
            ("disk_medium_percent", self.disk_medium),
            ("memory_available_high_percent", self.memory_available_high),
        ] {
            if !(0.0..=1.0).contains(&v) {
                problems.push(format!("{name} must be between 0 and 100"));
            }
        }
        if self.disk_medium > self.disk_high {
            problems.push(format!(
                "disk_medium_percent ({:.0}) is above disk_high_percent ({:.0}); when you move one, set the other too",
                self.disk_medium * 100.0,
                self.disk_high * 100.0
            ));
        }
        if self.load_medium <= 0.0 || self.load_medium > self.load_high {
            problems.push(format!(
                "load_medium_per_core ({}) must be positive and not above load_high_per_core ({}); when you move one, \
                 set the other too",
                self.load_medium, self.load_high
            ));
        }
        problems
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    /// Checked in order; the first rule that matches a change applies.
    pub rules: Vec<PolicyRule>,
    pub thresholds: Thresholds,
}

impl Policy {
    pub fn is_default(&self) -> bool {
        self.rules.is_empty() && self.thresholds == Thresholds::default()
    }

    pub fn find(&self, change: &Change) -> Option<&PolicyRule> {
        self.rules.iter().find(|r| r.matches(change))
    }

    /// Policy rules whose rule glob matches no built-in rule (usually typos).
    pub fn unknown_rules(&self) -> Vec<&PolicyRule> {
        self.rules.iter().filter(|r| !RULES.iter().any(|(id, _)| glob(&r.rule, id))).collect()
    }
}

/// Recorded on a change whose level a policy set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyNote {
    /// The level the built-in rule assigned.
    pub default: Significance,
    /// The policy rule that changed it.
    pub matched: String,
}

/// Every built-in rule id, with what it reports. `policy rules` lists these,
/// and policies are checked against them. A test keeps it in sync with the
/// rule code.
pub const RULES: &[(&str, &str)] = &[
    ("collector.failed", "a collector that worked before now fails"),
    ("system.hostname", "hostname changed"),
    ("system.os", "OS release changed"),
    ("system.kernel", "kernel version changed"),
    ("system.architecture", "machine architecture changed"),
    ("system.reboot", "the machine rebooted"),
    ("system.timezone", "timezone changed"),
    ("system.hardware", "DMI vendor or product changed"),
    ("system.container", "container runtime around Hostprint changed"),
    ("memory.total", "total memory changed"),
    ("memory.available", "available memory dropped (or rose)"),
    ("swap.total", "swap size changed"),
    ("swap.used", "swap use grew"),
    ("cpu.cores", "logical core count changed"),
    ("cpu.usage", "sampled CPU busy time jumped"),
    ("cpu.iowait", "sampled iowait jumped"),
    ("cpu.steal", "sampled steal time jumped"),
    ("load.average", "1-minute load per core crossed a threshold"),
    ("pressure.stall", "CPU, memory or I/O pressure stall crossed a threshold"),
    ("disk.unresponsive", "a filesystem stopped answering"),
    ("disk.responsive", "a filesystem answers again"),
    ("disk.read_only", "a filesystem was remounted read-only or read-write"),
    ("disk.usage", "disk usage crossed a threshold or grew"),
    ("disk.inodes", "inode usage crossed a threshold"),
    ("disk.device", "a mount point is backed by a different device"),
    ("disk.unmounted", "a filesystem is no longer mounted"),
    ("disk.mounted", "a new filesystem is mounted"),
    ("disk.size", "a filesystem's size changed"),
    ("process.disappeared", "no process with this name any more"),
    ("process.appeared", "a new process name"),
    ("process.count", "the number of instances changed a lot"),
    ("process.memory", "resident memory grew"),
    ("process.restarted", "a single-instance process restarted"),
    ("process.exe", "a single-instance process runs a different executable"),
    ("process.uninterruptible", "processes stuck in D state"),
    ("process.zombies", "zombie processes accumulated"),
    ("process.total", "the process count changed"),
    ("network.listener_removed", "a port is no longer listened on"),
    ("network.listener_added", "a new listening port"),
    ("network.listener_owner", "a different process owns a port"),
    ("network.listener_exposed", "a port is now bound to all interfaces"),
    ("network.listener_address", "a port's bind address changed"),
    ("network.interface_down", "a physical interface went down"),
    ("network.interface_state", "an interface's state changed"),
    ("network.interface_address", "an interface's addresses changed"),
    ("network.interface_removed", "an interface disappeared"),
    ("network.interface_added", "a new interface"),
    ("network.interface_mtu", "an interface's MTU changed"),
    ("network.interface_mac", "an interface's MAC address changed"),
    ("network.gateway", "default routes changed"),
    ("network.dns", "DNS servers changed"),
    ("network.dns_search", "DNS search domains changed"),
    ("network.tcp_state", "a surge in CLOSE_WAIT, SYN_SENT, TIME_WAIT, ... sockets"),
    ("service.failed", "a unit entered the failed state"),
    ("service.restart_loop", "a unit is waiting to be restarted"),
    ("service.stopped", "an active unit stopped"),
    ("service.restarts", "systemd restarted a unit"),
    ("service.restarted", "a unit was restarted by hand"),
    ("service.recovered", "a failed unit is active again"),
    ("service.started", "a unit became active"),
    ("service.state", "another unit state change"),
    ("service.removed", "a unit is no longer loaded"),
    ("service.added", "a new unit"),
    ("container.state", "a container's state changed"),
    ("container.health", "a container's health changed"),
    ("container.restarts", "a container's restart count grew"),
    ("container.oom_killed", "a container was OOM-killed"),
    ("container.memory_limit", "a container is near its memory limit"),
    ("container.memory", "a container's memory grew"),
    ("container.removed", "a container no longer exists"),
    ("container.added", "a new container"),
    ("container.exit_code", "a container exited with a new non-zero code"),
    ("container.image", "a container runs a different image"),
    ("container.image_id", "same image tag, different image"),
    ("container.ports", "a container's port bindings changed"),
    ("container.recreated", "same name, new container ID"),
    ("container.restarted", "a container was started again"),
    ("docker.engine", "Docker Engine version changed"),
    ("git.commit", "HEAD commit changed"),
    ("git.branch", "branch changed"),
    ("git.dirty", "the working tree has uncommitted changes"),
    ("git.clean", "the working tree is clean again"),
    ("git.changes", "the set of changed files differs"),
    ("git.remote", "the origin URL changed"),
    ("git.untracked", "the untracked file count changed"),
    ("git.root", "the snapshots describe different repositories"),
    ("runtime.version", "a runtime's version changed"),
    ("runtime.removed", "a runtime is no longer on PATH"),
    ("runtime.added", "a new runtime on PATH"),
    ("runtime.path", "a different binary is first on PATH"),
    ("env.changed", "a variable's value changed"),
    ("env.secret_changed", "a redacted value's fingerprint changed"),
    ("env.removed", "a variable is no longer set"),
    ("env.added", "a new variable"),
    ("env.path", "PATH changed"),
    ("env.redaction", "a value is redacted in one snapshot only"),
    ("file.removed", "a tracked file no longer exists"),
    ("file.content", "a tracked file's content changed"),
    ("file.created", "a tracked file now exists"),
    ("file.mode", "a file's permission bits changed"),
    ("file.owner", "a file's owner changed"),
    ("file.error", "a file could not be read in one snapshot"),
    ("file.touched", "a file's mtime changed, its content did not"),
    ("file.tracked", "a file is only configured in the newer snapshot"),
    ("file.untracked", "a file is only configured in the older snapshot"),
    ("log.errors", "more (or fewer) error lines in a log source"),
    ("log.new_error", "an error message not seen before"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use crate::{diff, DiffOptions, Significance::*};
    use std::collections::BTreeSet;

    fn rule(rule: &str, subject: Option<&str>, action: Action) -> PolicyRule {
        PolicyRule { rule: rule.into(), subject: subject.map(str::to_string), action, source: "test".into() }
    }

    fn with(rules: Vec<PolicyRule>) -> DiffOptions {
        DiffOptions { policy: Policy { rules, ..Default::default() }, ..Default::default() }
    }

    fn broken() -> (hostprint_model::Snapshot, hostprint_model::Snapshot) {
        let a = baseline();
        let mut b = later(&a, 600);
        let containers = &mut b.docker.as_mut().unwrap().containers;
        containers[1].health = Some("unhealthy".into()); // redis
        containers[0].id = "recreated000".into(); // api: container.recreated (LOW)
        b.runtimes.as_mut().unwrap()[0].version = Some("22.9.1".into()); // runtime.version (LOW)
        (a, b)
    }

    #[test]
    fn levels_can_be_changed_and_are_explained() {
        let (a, b) = broken();
        let d = diff(&a, &b, &with(vec![rule("runtime.*", None, Action::Level(High))]));
        let c = d.changes.iter().find(|c| c.rule == "runtime.version").unwrap();
        assert_eq!(c.significance, High);
        let note = c.policy.as_ref().unwrap();
        assert_eq!((note.default, note.matched.as_str()), (Low, "runtime.* (test)"));
        assert_eq!(d.summary.high, 2, "summary counts use the policy level");
        assert_eq!(d.changes[0].rule, "container.health", "sorting still by level, then category");
    }

    #[test]
    fn rules_can_be_turned_off_with_a_note() {
        let (a, b) = broken();
        let d = diff(&a, &b, &with(vec![rule("container.recreated", None, Action::Off)]));
        assert!(d.changes.iter().all(|c| c.rule != "container.recreated"));
        assert!(
            d.notes.iter().any(|n| n.contains("turned off 1 change") && n.contains("container.recreated ×1")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn subjects_scope_rules_and_the_first_match_wins() {
        let (a, b) = broken();
        let opts =
            with(vec![rule("container.*", Some("redis"), Action::Level(Info)), rule("container.*", None, Action::Off)]);
        let d = diff(&a, &b, &opts);
        let health = d.changes.iter().find(|c| c.rule == "container.health").unwrap();
        assert_eq!(health.significance, Info);
        assert!(d.changes.iter().all(|c| c.rule != "container.recreated"), "the catch-all turned api's change off");
    }

    #[test]
    fn thresholds_move_the_limits() {
        let a = baseline();
        let mut b = later(&a, 600);
        let disk = &mut b.resources.as_mut().unwrap().disks[0];
        disk.used_bytes = Some(83 * GIB); // 83 / 95 = 87%
        disk.available_bytes = Some(12 * GIB);
        let default = diff(&a, &b, &DiffOptions::default());
        assert_eq!(default.changes[0].significance, Low);
        let strict = DiffOptions {
            policy: Policy {
                thresholds: Thresholds { disk_high: 0.85, disk_medium: 0.80, ..Default::default() },
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(diff(&a, &b, &strict).changes[0].significance, High);
    }

    #[test]
    fn validates_thresholds_and_finds_typos() {
        let bad = Thresholds { disk_high: 0.8, disk_medium: 0.9, load_medium: 3.0, ..Default::default() };
        assert_eq!(bad.validate().len(), 2);
        assert!(Thresholds::default().validate().is_empty());
        let p = Policy {
            rules: vec![rule("contianer.*", None, Action::Off), rule("container.*", None, Action::Off)],
            ..Default::default()
        };
        let unknown: Vec<&str> = p.unknown_rules().iter().map(|r| r.rule.as_str()).collect();
        assert_eq!(unknown, ["contianer.*"]);
        assert_eq!(Action::parse("OFF"), Some(Action::Off));
        assert_eq!(Action::parse("medium"), Some(Action::Level(Medium)));
        assert_eq!(Action::parse("severe"), None);
    }

    /// Every rule id the code can produce is in RULES, and nothing else is.
    #[test]
    fn rule_registry_matches_the_code() {
        let sources = [
            include_str!("lib.rs"),
            include_str!("system.rs"),
            include_str!("resources.rs"),
            include_str!("processes.rs"),
            include_str!("network.rs"),
            include_str!("services.rs"),
            include_str!("containers.rs"),
            include_str!("application.rs"),
            include_str!("configuration.rs"),
            include_str!("files.rs"),
            include_str!("logs.rs"),
        ];
        let mut in_code = BTreeSet::new();
        for source in sources {
            let code = source.split("#[cfg(test)]").next().unwrap();
            for literal in code.split('"').skip(1).step_by(2) {
                let is_rule_id = literal.split_once('.').is_some_and(|(a, b)| {
                    !a.is_empty()
                        && !b.is_empty()
                        && a.chars().all(|c| c.is_ascii_lowercase())
                        && b.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                });
                if is_rule_id {
                    in_code.insert(literal.to_string());
                }
            }
        }
        let registered: BTreeSet<String> = RULES.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(in_code, registered);
    }
}
