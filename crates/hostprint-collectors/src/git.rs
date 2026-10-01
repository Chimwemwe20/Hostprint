//! Git metadata for the application in the working directory.
//!
//! Only metadata is recorded: commit, branch, which tracked files changed.
//! File contents are never read. Commands run with `GIT_OPTIONAL_LOCKS=0` so
//! inspecting a repository never rewrites its index.

use crate::util::{run_command, which, CommandOutput};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::Git;

const MAX_CHANGED_PATHS: usize = 100;
const MAX_SUBJECT: usize = 120;

pub struct GitCollector;

impl Collector for GitCollector {
    fn name(&self) -> &'static str {
        "git"
    }

    fn title(&self) -> &'static str {
        "Git"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        let git = which("git").ok_or_else(|| CollectError::Unavailable("git not installed".into()))?;
        let dir = ctx.repo_dir.as_path();
        let run = |args: &[&str]| -> Result<CommandOutput, CollectError> {
            Ok(run_command(&git, args, Some(dir), ctx.command_timeout)?)
        };

        let top = run(&["rev-parse", "--show-toplevel"])?;
        if !top.success {
            let err = top.error_line();
            return Err(if err.contains("not a git repository") {
                CollectError::Unavailable(format!("{} is not inside a Git repository", dir.display()))
            } else if err.contains("dubious ownership") {
                CollectError::Failed(format!(
                    "{}: repository owned by another user (see `git config --global safe.directory`)",
                    dir.display()
                ))
            } else {
                CollectError::Failed(err)
            });
        }
        let root = top.stdout.trim().to_string();
        let ok_line = |out: CommandOutput| out.success.then(|| out.stdout.trim().to_string()).filter(|s| !s.is_empty());

        let commit = ok_line(run(&["rev-parse", "--verify", "-q", "HEAD"])?);
        let branch = ok_line(run(&["symbolic-ref", "-q", "--short", "HEAD"])?);
        let (commit_subject, commit_time) = match commit {
            Some(_) => match ok_line(run(&["log", "-1", "--format=%s%x1f%cI"])?) {
                Some(line) => {
                    let (subject, time) = line.split_once('\u{1f}').unwrap_or((line.as_str(), ""));
                    (Some(truncate(subject, MAX_SUBJECT)), (!time.is_empty()).then(|| time.to_string()))
                }
                None => (None, None),
            },
            None => (None, None),
        };
        let describe = ok_line(run(&["describe", "--tags", "--abbrev=7"])?);
        let remote = ok_line(run(&["remote", "get-url", "origin"])?).map(|url| ctx.redactor.value(&url));

        let status = run(&["status", "--porcelain=v1", "-z", "--untracked-files=normal"])?;
        if !status.success {
            return Err(CollectError::Failed(format!("git status: {}", status.error_line())));
        }
        let parsed = parse_status(&status.stdout);

        let git = Git {
            root,
            branch,
            commit,
            commit_subject,
            commit_time,
            describe,
            remote,
            dirty: parsed.staged + parsed.modified > 0,
            staged: parsed.staged,
            modified: parsed.modified,
            untracked: parsed.untracked,
            changed_paths: parsed.paths,
        };
        let summary = format!(
            "{} @ {} · {}",
            git.branch.as_deref().unwrap_or("(detached)"),
            git.short_commit().unwrap_or("(no commits)"),
            if git.dirty { "dirty" } else { "clean" }
        );
        Ok(Collected::new(Section::Git(git)).summary(summary))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Status {
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub paths: Vec<String>,
}

/// Parses `git status --porcelain=v1 -z`.
pub(crate) fn parse_status(stdout: &str) -> Status {
    let mut status = Status::default();
    let mut entries = stdout.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else { continue };
        let mut chars = code.chars();
        let (x, y) = (chars.next().unwrap_or(' '), chars.next().unwrap_or(' '));
        if x == '?' {
            status.untracked += 1;
            continue;
        }
        if x == '!' {
            continue;
        }
        if x != ' ' {
            status.staged += 1;
        }
        if y != ' ' {
            status.modified += 1;
        }
        // Renames and copies are followed by the original path.
        if x == 'R' || x == 'C' {
            entries.next();
        }
        if status.paths.len() < MAX_CHANGED_PATHS {
            status.paths.push(path.to_string());
        }
    }
    status.paths.sort();
    status
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_status() {
        let out = " M src/main.rs\0M  Cargo.toml\0MM README.md\0R  new.rs\0old.rs\0?? scratch.txt\0?? notes/\0";
        let s = parse_status(out);
        assert_eq!(s.staged, 3);
        assert_eq!(s.modified, 2);
        assert_eq!(s.untracked, 2);
        assert_eq!(s.paths, ["Cargo.toml", "README.md", "new.rs", "src/main.rs"]);
        assert_eq!(parse_status(""), Status::default());
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("héllo", 3), "hél…");
        assert_eq!(truncate("hi", 3), "hi");
    }
}
