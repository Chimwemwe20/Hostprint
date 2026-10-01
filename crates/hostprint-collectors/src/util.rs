use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    /// The first non-empty line of stderr, or of stdout if stderr is empty.
    pub fn error_line(&self) -> String {
        self.stderr
            .lines()
            .chain(self.stdout.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("command failed")
            .to_string()
    }
}

/// Runs a command with a hard timeout, never blocking on a full pipe.
///
/// The environment is pinned so output is parseable and the command never
/// waits for input: C locale, no pager, no Git prompts, no Git index locks.
pub fn run_command(
    program: impl AsRef<Path>,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> io::Result<CommandOutput> {
    let mut cmd = Command::new(program.as_ref());
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C")
        .env("PAGER", "cat")
        .env("SYSTEMD_PAGER", "")
        .env("SYSTEMD_COLORS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // Don't join the readers: a grandchild may still hold the pipes.
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("timed out after {}s", timeout.as_secs_f32())));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let join = |h: Option<JoinHandle<String>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(CommandOutput { success: status.success(), code: status.code(), stdout: join(stdout), stderr: join(stderr) })
}

fn drain<R: Read + Send + 'static>(mut reader: R) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Finds an executable on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Reads a file and trims surrounding whitespace; `None` if unreadable or empty.
pub(crate) fn read_trimmed(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Maps UIDs to user names from an `/etc/passwd`-format file.
pub(crate) fn parse_passwd(contents: &str) -> HashMap<u32, String> {
    contents
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid = fields.nth(1)?.parse().ok()?;
            Some((uid, name.to_string()))
        })
        .collect()
}

/// Effective UID of this process.
pub(crate) fn euid() -> Option<u32> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        Some(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

pub(crate) fn is_root() -> bool {
    euid() == Some(0)
}

/// Kernel clock ticks per second, used to interpret `/proc/<pid>/stat` times.
pub(crate) fn clock_ticks() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no preconditions.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 {
            return ticks as u64;
        }
    }
    100
}

/// Truncates to at most `max` bytes on a character boundary, marking the cut.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

pub(crate) fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub(crate) fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passwd() {
        let users = parse_passwd(
            "root:x:0:0:root:/root:/bin/bash\n# comment\ndeploy:x:1000:1000::/home/deploy:/bin/sh\nbroken\n",
        );
        assert_eq!(users.get(&0).map(String::as_str), Some("root"));
        assert_eq!(users.get(&1000).map(String::as_str), Some("deploy"));
        assert_eq!(users.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn runs_commands_and_times_out() {
        let out = run_command("sh", &["-c", "echo hi; echo err >&2; exit 3"], None, Duration::from_secs(5)).unwrap();
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "err");
        assert_eq!(out.code, Some(3));
        assert!(!out.success);

        let err = run_command("sh", &["-c", "sleep 5"], None, Duration::from_millis(100)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
