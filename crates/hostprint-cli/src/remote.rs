//! Capturing another machine over SSH, without installing anything on it.
//!
//! The system `ssh` client does the connecting, so the user's keys, agent,
//! `~/.ssh/config` and known_hosts apply unchanged. One connection checks the
//! remote is Linux on a matching architecture; a second streams a tar archive
//! (the hostprint binary plus a fingerprint key) into a private temporary
//! directory, runs a capture there, prints the snapshot as JSON and removes the
//! directory, even when the capture fails.
//!
//! Nothing secret appears on a command line, where other users of the remote
//! could see it with `ps`: the key travels inside the archive on stdin. The
//! remote script itself is sent base64-encoded, so it survives any login shell.

use crate::{App, CaptureOptions};
use anyhow::{bail, Context, Result};
use hostprint_collectors::redact::hmac_sha256;
use hostprint_core::Config;
use hostprint_model::Snapshot;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Where `ssh://[user@]host[:port]` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl Target {
    /// `None` if `reference` is not an `ssh://` URL; an error if it is a
    /// malformed one.
    pub fn parse(reference: &str) -> Option<Result<Target>> {
        let rest = reference.strip_prefix("ssh://")?;
        Some(Self::parse_authority(rest.trim_end_matches('/')))
    }

    fn parse_authority(s: &str) -> Result<Target> {
        let (user, hostport) = match s.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, s),
        };
        // [v6::addr]:port, host:port or host
        let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
            let (h, after) = rest.split_once(']').context("unclosed '[' in the SSH host")?;
            (h.to_string(), after.strip_prefix(':'))
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (hostport.to_string(), None),
            }
        };
        // A leading '-' would make ssh read the destination as an option.
        let valid = |s: &str| {
            !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-:".contains(c))
        };
        if !valid(&host) || user.as_deref().is_some_and(|u| !valid(u)) {
            bail!("'{s}' is not a valid SSH destination (expected ssh://[user@]host[:port])");
        }
        let port = port.map(|p| p.parse::<u16>().with_context(|| format!("invalid SSH port '{p}'"))).transpose()?;
        Ok(Target { user, host, port })
    }

    fn destination(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn url(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        match &self.user {
            Some(u) => format!("ssh://{u}@{host}{port}"),
            None => format!("ssh://{host}{port}"),
        }
    }

    fn ssh(&self) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args(["-T", "-o", "ConnectTimeout=15"]);
        if let Some(port) = self.port {
            cmd.args(["-p", &port.to_string()]);
        }
        // `--` ends ssh's options; everything after the destination is the
        // remote command.
        cmd.arg("--").arg(self.destination());
        cmd
    }
}

/// Captures `target` and returns the snapshot (not saved).
pub fn capture(app: &App, config: &Config, target: &Target, name: &str, options: &CaptureOptions) -> Result<Snapshot> {
    let binary_path = match &options.remote_binary {
        Some(p) => p.clone(),
        None => std::env::current_exe().context("locating the hostprint binary to send")?,
    };
    preflight(target, options.remote_binary.is_none())?;
    let binary = std::fs::read(&binary_path).with_context(|| format!("reading {}", binary_path.display()))?;

    // Per-host key: captures of the same server can compare secret
    // fingerprints, and no remote ever holds the local key.
    let local_key = app.store.fingerprint_key()?;
    let key = hmac_sha256(&local_key, format!("ssh:{}", target.host.to_ascii_lowercase()).as_bytes());
    let archive = payload(&binary, &key)?;
    let script = remote_script(name, &forwarded_args(config, options));

    let mut child = target
        .ssh()
        .arg(format!("sh -c 'eval \"$(echo {} | base64 -d)\"'", base64(script.as_bytes())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running ssh (is an SSH client installed?)")?;
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || stdin.write_all(&archive));
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stdout = Vec::new();
    child.stdout.take().expect("piped").read_to_end(&mut stdout)?;
    let status = child.wait()?;
    let _ = writer.join();
    let stderr = errors.join().unwrap_or_default();

    if !status.success() {
        let detail = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no error output").trim();
        let mut message = format!("remote capture on {} failed: {detail}", target.url());
        if stderr.contains("GLIBC") || (stderr.contains("not found") && stderr.contains("hostprint")) {
            message.push_str(
                "\nhint: send a static build with --remote-binary (docker build --target binary --output dist .)",
            );
        } else if stderr.contains("Permission denied") {
            message.push_str(
                "\nhint: if /tmp is mounted noexec on the remote, set TMPDIR there to an executable directory",
            );
        }
        bail!(message);
    }
    let mut snapshot: Snapshot = serde_json::from_slice(&stdout)
        .with_context(|| format!("{} returned something that is not a snapshot", target.url()))?;
    snapshot.capture.remote = Some(target.url());
    Ok(snapshot)
}

/// Checks the connection and that the remote can run our binary.
fn preflight(target: &Target, check_arch: bool) -> Result<()> {
    let out = target.ssh().arg("uname -sm").stdin(Stdio::null()).output().context("running ssh")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("cannot reach {}: {}", target.url(), stderr.trim().lines().last().unwrap_or("ssh failed"));
    }
    let uname = String::from_utf8_lossy(&out.stdout);
    let mut parts = uname.split_whitespace();
    let (os, arch) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if os != "Linux" {
        bail!("{} runs {os}; remote capture supports Linux", target.url());
    }
    if check_arch && arch != std::env::consts::ARCH {
        bail!(
            "{} is {arch} and this hostprint is {}; pass a {arch} build with --remote-binary",
            target.url(),
            std::env::consts::ARCH
        );
    }
    Ok(())
}

/// The archive sent on stdin: the binary, and the home directory with the key.
fn payload(binary: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut add = |path: &str, mode: u32, data: &[u8], kind: tar::EntryType| -> std::io::Result<()> {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_mtime(0);
        archive.append_data(&mut header, path, data)
    };
    add("hostprint", 0o700, binary, tar::EntryType::Regular)?;
    add("home/", 0o700, &[], tar::EntryType::Directory)?;
    add("home/fingerprint.key", 0o600, key, tar::EntryType::Regular)?;
    Ok(archive.into_inner()?)
}

/// Capture options passed on to the remote capture. `config.toml` stays
/// local, except the parts that decide what to collect.
fn forwarded_args(config: &Config, options: &CaptureOptions) -> Vec<String> {
    let mut args = Vec::new();
    let logs_since =
        options.logs_since.clone().or_else(|| config.hostprint.collect_logs.then(|| config.logs.since.clone()));
    if let Some(since) = logs_since {
        args.extend(["--logs-since".into(), since]);
    }
    if !options.only.is_empty() {
        args.extend(["--only".into(), options.only.join(",")]);
    }
    let skip: Vec<String> = config.collectors.disable.iter().chain(&options.skip).cloned().collect();
    if !skip.is_empty() {
        args.extend(["--skip".into(), skip.join(",")]);
    }
    if let Some(repo) = &options.repo {
        args.extend(["--repo".into(), path_arg(repo)]);
    }
    for f in &options.env_files {
        args.extend(["--env-file".into(), path_arg(f)]);
    }
    for f in &options.files {
        args.extend(["--file".into(), path_arg(f)]);
    }
    args
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The script run on the remote, under `sh`.
pub(crate) fn remote_script(name: &str, args: &[String]) -> String {
    let args: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    format!(
        "set -e\n\
         umask 077\n\
         d=$(mktemp -d \"${{TMPDIR:-/tmp}}/hostprint.XXXXXX\")\n\
         trap 'rm -rf \"$d\"' EXIT INT TERM HUP\n\
         tar -x -C \"$d\" -f -\n\
         \"$d/hostprint\" --home \"$d/home\" --no-color capture --json --no-save --quiet --name {} {}\n",
        sh_quote(name),
        args.join(" ")
    )
}

/// Quotes a string for a POSIX shell.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Standard base64 with padding.
pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_targets() {
        assert!(Target::parse("healthy").is_none());
        let t = Target::parse("ssh://deploy@web-1.internal:2222").unwrap().unwrap();
        assert_eq!(t, Target { user: Some("deploy".into()), host: "web-1.internal".into(), port: Some(2222) });
        assert_eq!(t.url(), "ssh://deploy@web-1.internal:2222");
        let t = Target::parse("ssh://web-2").unwrap().unwrap();
        assert_eq!((t.user.as_deref(), t.port, t.destination()), (None, None, "web-2".to_string()));
        let t = Target::parse("ssh://root@[fd00::1]:22/").unwrap().unwrap();
        assert_eq!((t.host.as_str(), t.port), ("fd00::1", Some(22)));
        assert_eq!(t.url(), "ssh://root@[fd00::1]:22");
        for bad in [
            "ssh://",
            "ssh://host:99999",
            "ssh://a b",
            "ssh://host;rm -rf",
            "ssh://-oProxyCommand=x",
            "ssh://-oProxyCommand",
            "ssh://-l@host",
        ] {
            assert!(Target::parse(bad).unwrap().is_err(), "{bad}");
        }
    }

    #[test]
    fn quotes_and_encodes() {
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"hostprint"), "aG9zdHByaW50");
        let script = remote_script("web-1 'prod'", &["--repo".into(), "/srv/my app".into()]);
        assert!(script.contains("--name 'web-1 '\\''prod'\\''' '--repo' '/srv/my app'"), "{script}");
        assert!(script.contains("trap 'rm -rf \"$d\"' EXIT"));
    }

    #[test]
    fn payload_carries_binary_and_key_with_private_modes() {
        let archive = payload(b"\x7fELF...", &[7u8; 32]).unwrap();
        let mut entries: Vec<(String, u32, usize)> = tar::Archive::new(archive.as_slice())
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.path().unwrap().display().to_string(),
                    e.header().mode().unwrap(),
                    e.header().size().unwrap() as usize,
                )
            })
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            [("home/".into(), 0o700, 0), ("home/fingerprint.key".into(), 0o600, 32), ("hostprint".into(), 0o700, 7)]
        );
    }
}
