//! `config.toml`: what to collect, what to redact, what to ignore when diffing.
//!
//! Every field is optional; a missing file means defaults. Unknown keys are
//! ignored so that configuration written for newer versions still loads.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hostprint: General,
    pub collectors: Collectors,
    pub files: Files,
    pub env: Env,
    pub logs: LogsConfig,
    pub redact: Redact,
    pub ignore: Ignore,
    pub policy: PolicyConfig,
}

/// `[policy]` in `config.toml`, or the whole of a `--policy` file: level
/// overrides for diff rules, and thresholds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Checked in order; the first entry matching a change applies.
    pub rules: Vec<PolicyRuleConfig>,
    pub thresholds: ThresholdsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRuleConfig {
    /// Rule id, `*` wildcards allowed: "container.*".
    pub rule: String,
    /// Only changes about this subject, `*` wildcards allowed.
    #[serde(default)]
    pub subject: Option<String>,
    /// "off", "info", "low", "medium" or "high".
    pub level: String,
}

/// Percentages and load ratios; unset values keep the built-in defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThresholdsConfig {
    pub disk_high_percent: Option<f64>,
    pub disk_medium_percent: Option<f64>,
    pub memory_available_high_percent: Option<f64>,
    pub load_high_per_core: Option<f64>,
    pub load_medium_per_core: Option<f64>,
}

impl PolicyConfig {
    /// Loads a standalone policy file: `[[rules]]` and `[thresholds]` at the
    /// top level, the same shape as `[policy]` in `config.toml`.
    pub fn load(path: &Path) -> Result<PolicyConfig, ConfigError> {
        let err = |message: String| ConfigError { path: path.to_path_buf(), message };
        let contents = std::fs::read_to_string(path).map_err(|e| err(e.to_string()))?;
        toml::from_str(&contents).map_err(|e| err(e.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Turning this off stores secret values verbatim. Not recommended.
    pub redact_secrets: bool,
    /// Collect recent logs on every capture (`capture --logs-since` does it
    /// for one capture).
    pub collect_logs: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collectors {
    /// Collectors to turn off, e.g. `["runtimes"]`.
    pub disable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsConfig {
    /// How far back to read, e.g. "30m" or "2h".
    #[serde(alias = "journal_since")]
    pub since: String,
    /// systemd journal entries at warning level and worse.
    pub journal: bool,
    /// Output of Docker containers.
    pub docker: bool,
    /// Plain log files; only their last 256 KiB is read.
    pub files: Vec<PathBuf>,
    /// Lines kept per source.
    pub lines: usize,
}

impl Default for LogsConfig {
    fn default() -> Self {
        LogsConfig { since: "30m".into(), journal: true, docker: true, files: Vec::new(), lines: 50 }
    }
}

/// Parses a duration such as "90s", "30m", "2h", "1d" or "1h30m".
pub fn parse_duration(input: &str) -> Result<std::time::Duration, String> {
    let s = input.trim();
    let invalid = || format!("invalid duration '{input}' (use e.g. 30m, 2h, 1h30m)");
    if s.is_empty() {
        return Err(invalid());
    }
    let mut total = 0u64;
    let mut number = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let n: u64 = number.parse().map_err(|_| invalid())?;
        number.clear();
        total += n * match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(invalid()),
        };
    }
    if !number.is_empty() || total == 0 {
        return Err(invalid());
    }
    Ok(std::time::Duration::from_secs(total))
}

impl Default for General {
    fn default() -> Self {
        General { redact_secrets: true, collect_logs: false }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Files {
    /// Files to fingerprint (size, mtime, mode, SHA-256).
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Env {
    /// Record Hostprint's own environment.
    pub capture_process: bool,
    /// Dotenv files whose variables are recorded, e.g. `/srv/app/.env`.
    pub files: Vec<PathBuf>,
}

impl Default for Env {
    fn default() -> Self {
        Env { capture_process: true, files: Vec::new() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Redact {
    /// Extra name fragments that mark a variable as secret.
    pub patterns: Vec<String>,
    /// Variable names that are never redacted by name.
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ignore {
    pub processes: Vec<String>,
    pub ports: Vec<u16>,
    pub env: Vec<String>,
    pub containers: Vec<String>,
    pub services: Vec<String>,
}

#[derive(Debug)]
pub struct ConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Loads `path`, or returns defaults if it does not exist.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                Config::parse(&contents).map_err(|message| ConfigError { path: path.to_path_buf(), message })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(ConfigError { path: path.to_path_buf(), message: e.to_string() }),
        }
    }

    pub fn parse(contents: &str) -> Result<Config, String> {
        toml::from_str(contents).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_example() {
        let config = Config::parse(
            r#"
            [hostprint]
            redact_secrets = true
            collect_logs = false

            [files]
            paths = ["/etc/nginx/nginx.conf"]

            [ignore]
            processes = ["chrome"]
            ports = [5353]

            [future_section]
            whatever = 1
            "#,
        )
        .unwrap();
        assert!(config.hostprint.redact_secrets);
        assert_eq!(config.files.paths, [PathBuf::from("/etc/nginx/nginx.conf")]);
        assert_eq!(config.ignore.processes, ["chrome"]);
        assert_eq!(config.ignore.ports, [5353]);
        assert!(config.env.capture_process, "unspecified sections keep defaults");
    }

    #[test]
    fn parses_logs_and_collectors() {
        let config = Config::parse(
            "[hostprint]\ncollect_logs = true\n[logs]\njournal_since = \"20m\"\nfiles = [\"/var/log/app.log\"]\n\
             [collectors]\ndisable = [\"runtimes\"]\n",
        )
        .unwrap();
        assert!(config.hostprint.collect_logs);
        assert_eq!(config.logs.since, "20m", "the design doc's journal_since name works");
        assert!(config.logs.docker);
        assert_eq!(config.collectors.disable, ["runtimes"]);
    }

    #[test]
    fn parses_policies() {
        let config = Config::parse(
            "[[policy.rules]]\nrule = \"container.recreated\"\nlevel = \"off\"\n\n\
             [[policy.rules]]\nrule = \"container.*\"\nsubject = \"payments-*\"\nlevel = \"high\"\n\n\
             [policy.thresholds]\ndisk_high_percent = 85\n",
        )
        .unwrap();
        assert_eq!(config.policy.rules.len(), 2);
        assert_eq!(config.policy.rules[1].subject.as_deref(), Some("payments-*"));
        assert_eq!(config.policy.thresholds.disk_high_percent, Some(85.0));
        assert_eq!(config.policy.thresholds.load_high_per_core, None);

        let dir = std::env::temp_dir().join(format!("hostprint-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("team.toml");
        std::fs::write(&file, "[[rules]]\nrule = \"git.*\"\nlevel = \"low\"\n[thresholds]\nload_high_per_core = 4\n")
            .unwrap();
        let p = PolicyConfig::load(&file).unwrap();
        assert_eq!((p.rules[0].rule.as_str(), p.thresholds.load_high_per_core), ("git.*", Some(4.0)));
        std::fs::write(&file, "[[rules]]\nrule = \"git.*\"\n").unwrap();
        assert!(PolicyConfig::load(&file).unwrap_err().to_string().contains("level"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_durations() {
        let secs = |s: &str| parse_duration(s).map(|d| d.as_secs());
        assert_eq!(secs("90s"), Ok(90));
        assert_eq!(secs("30m"), Ok(1800));
        assert_eq!(secs("1h30m"), Ok(5400));
        assert_eq!(secs("2d"), Ok(172_800));
        for bad in ["", "30", "m", "5x", "0m"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn empty_config_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert!(Config::parse("[ignore]\nports = \"oops\"").is_err());
    }
}
