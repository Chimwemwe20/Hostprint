//! Environment variables, from Hostprint's own process and configured env files.

use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::{EnvVar, Environment};

/// Longer values are stored as a fingerprint only (`LS_COLORS` and friends).
const MAX_VALUE: usize = 1024;

pub struct EnvironmentCollector;

impl Collector for EnvironmentCollector {
    fn name(&self) -> &'static str {
        "environment"
    }

    fn title(&self) -> &'static str {
        "Configuration"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        if !ctx.capture_process_env && ctx.env_files.is_empty() {
            return Err(CollectError::Unavailable("disabled in configuration".into()));
        }
        let mut variables = Vec::new();
        let mut notes = Vec::new();
        if ctx.capture_process_env {
            for (name, value) in std::env::vars_os() {
                let (name, value) = (name.to_string_lossy(), value.to_string_lossy());
                variables.push(record(ctx, &name, &value, "process"));
            }
        }
        for file in &ctx.env_files {
            let source = file.display().to_string();
            match std::fs::read_to_string(file) {
                Ok(contents) => {
                    for (name, value) in parse_dotenv(&contents) {
                        variables.push(record(ctx, &name, &value, &source));
                    }
                }
                Err(e) => notes.push(format!("{source}: {e}")),
            }
        }
        variables.sort_by(|a, b| (&a.source, &a.name).cmp(&(&b.source, &b.name)));
        variables.dedup_by(|a, b| a.source == b.source && a.name == b.name);

        let redacted = variables.iter().filter(|v| v.redacted).count();
        let summary = format!("{} variables · {} redacted", variables.len(), redacted);
        let env = Environment { fingerprint_key_id: ctx.redactor.key_id().to_string(), variables };
        let mut collected = Collected::new(Section::Environment(env)).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }
}

fn record(ctx: &CaptureContext, name: &str, value: &str, source: &str) -> EnvVar {
    let r = ctx.redactor.pair(name, value);
    let (value, fingerprint) = if !r.redacted && r.value.len() > MAX_VALUE {
        (format!("[{} bytes]", r.value.len()), Some(ctx.redactor.fingerprint(value)))
    } else {
        (r.value, r.fingerprint)
    };
    EnvVar { name: name.to_string(), source: source.to_string(), value, redacted: r.redacted, fingerprint }
}

/// Parses a dotenv file: `KEY=value`, optional `export`, quotes and comments.
pub(crate) fn parse_dotenv(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
                return None;
            }
            let value = value.trim();
            let value = match value.chars().next() {
                Some(q @ ('"' | '\'')) => match value[1..].find(q) {
                    Some(end) => &value[1..=end],
                    None => &value[1..],
                },
                _ => value.split(" #").next().unwrap_or("").trim_end(),
            };
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Redactor;

    #[test]
    fn parses_dotenv_files() {
        let parsed = parse_dotenv(
            "# app config\nDATABASE_POOL_SIZE=20\nexport DATABASE_HOST=db.internal # primary\n\
             GREETING=\"hello # not a comment\"\nSINGLE='x'\nEMPTY=\nnot a line\n",
        );
        assert_eq!(
            parsed,
            [
                ("DATABASE_POOL_SIZE".into(), "20".into()),
                ("DATABASE_HOST".into(), "db.internal".into()),
                ("GREETING".into(), "hello # not a comment".into()),
                ("SINGLE".into(), "x".into()),
                ("EMPTY".into(), String::new()),
            ]
        );
    }

    #[test]
    fn records_env_files_with_redaction() {
        let dir = std::env::temp_dir().join(format!("hostprint-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".env");
        let long = "x".repeat(MAX_VALUE + 1);
        std::fs::write(&file, format!("JWT_SECRET=abc\nPOOL=10\nLONG={long}\n")).unwrap();

        let mut ctx = CaptureContext::new(Redactor::new(b"k"));
        ctx.capture_process_env = false;
        ctx.env_files = vec![file.clone()];
        let Section::Environment(env) = EnvironmentCollector.collect(&ctx).unwrap().section else {
            panic!("wrong section")
        };
        let get = |n: &str| env.variables.iter().find(|v| v.name == n).unwrap();
        assert!(get("JWT_SECRET").redacted);
        assert_eq!(get("JWT_SECRET").value, "[REDACTED]");
        assert_eq!(get("POOL").value, "10");
        assert_eq!(get("LONG").value, format!("[{} bytes]", MAX_VALUE + 1));
        assert!(get("LONG").fingerprint.is_some());
        assert_eq!(get("POOL").source, file.display().to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
