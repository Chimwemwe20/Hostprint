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
    pub files: Files,
    pub env: Env,
    pub redact: Redact,
    pub ignore: Ignore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Turning this off stores secret values verbatim. Not recommended.
    pub redact_secrets: bool,
    /// Reserved for log collection (planned for v0.2).
    pub collect_logs: bool,
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
    fn empty_config_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert!(Config::parse("[ignore]\nports = \"oops\"").is_err());
    }
}
