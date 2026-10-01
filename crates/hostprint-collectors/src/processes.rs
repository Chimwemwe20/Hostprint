//! Process table from `/proc`.

use crate::system::parse_boot_time;
use crate::util::{clock_ticks, is_root, parse_passwd, round1};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use chrono::DateTime;
use hostprint_model::{Process, Processes};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// `PF_KTHREAD` from `include/linux/sched.h`.
const PF_KTHREAD: u64 = 0x0020_0000;
const MAX_CMDLINE: usize = 1024;

pub struct ProcessCollector;

impl Collector for ProcessCollector {
    fn name(&self) -> &'static str {
        "processes"
    }

    fn title(&self) -> &'static str {
        "Processes"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        #[cfg(target_os = "macos")]
        if ctx.is_live() {
            return crate::macos::live::processes(ctx);
        }
        ctx.require_linux()?;
        let proc_dir = ctx.path("/proc");
        let boot = std::fs::read_to_string(ctx.path("/proc/stat"))
            .ok()
            .and_then(|s| parse_boot_time(&s))
            .map(|t| t.timestamp());
        let ticks_per_sec = clock_ticks();
        let users = std::fs::read_to_string(ctx.path("/etc/passwd")).map(|s| parse_passwd(&s)).unwrap_or_default();

        // First pass: CPU ticks only, to measure usage over the sample window.
        let sample_start = Instant::now();
        let first: HashMap<u32, u64> = list_pids(&proc_dir)?
            .into_iter()
            .filter_map(|pid| {
                let stat = std::fs::read_to_string(proc_dir.join(pid.to_string()).join("stat")).ok()?;
                Some((pid, parse_stat(&stat)?.cpu_ticks))
            })
            .collect();
        std::thread::sleep(ctx.sample_interval);
        let wall = sample_start.elapsed().as_secs_f64();

        let mut list = Vec::new();
        let mut kernel_threads = 0;
        let mut exe_hidden = 0;
        for pid in list_pids(&proc_dir)? {
            let dir = proc_dir.join(pid.to_string());
            // Processes can exit at any point; skip whatever vanished.
            let Some(stat) = std::fs::read_to_string(dir.join("stat")).ok().and_then(|s| parse_stat(&s)) else {
                continue;
            };
            let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
            if stat.flags & PF_KTHREAD != 0 || (cmdline.is_empty() && (stat.ppid == 2 || pid == 2)) {
                kernel_threads += 1;
                continue;
            }
            let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
            let (uid, rss) = parse_status(&status);
            let exe = std::fs::read_link(dir.join("exe")).ok().map(|p| {
                let p = p.to_string_lossy().into_owned();
                p.strip_suffix(" (deleted)").map(str::to_string).unwrap_or(p)
            });
            if exe.is_none() && !cmdline.is_empty() {
                exe_hidden += 1;
            }
            let cpu_percent = first.get(&pid).filter(|_| wall > 0.0).map(|before| {
                let delta = stat.cpu_ticks.saturating_sub(*before) as f64 / ticks_per_sec as f64;
                round1(delta / wall * 100.0)
            });
            let started_at = boot
                .map(|b| b + (stat.start_ticks / ticks_per_sec) as i64)
                .and_then(|secs| DateTime::from_timestamp(secs, 0));
            list.push(Process {
                pid,
                ppid: stat.ppid,
                name: stat.comm,
                exe,
                cmdline: format_cmdline(ctx, &cmdline),
                user: uid.and_then(|u| users.get(&u).cloned()),
                uid,
                state: stat.state,
                cpu_percent,
                memory_bytes: rss.unwrap_or(0),
                threads: stat.threads,
                started_at,
            });
        }

        if let Some(self_pid) = ctx.self_pid {
            let excluded = subtree(&list, self_pid);
            list.retain(|p| !excluded.contains(&p.pid));
        }
        list.sort_by_key(|p| p.pid);

        let summary = format!("{} {}", list.len(), if list.len() == 1 { "process" } else { "processes" });
        let mut collected = Collected::new(Section::Processes(Processes { list, kernel_threads })).summary(summary);
        if exe_hidden > 0 && ctx.is_live() && !is_root() {
            collected = collected.note(format!(
                "executable paths unavailable for {exe_hidden} processes owned by other users (run as root for full details)"
            ));
        }
        Ok(collected)
    }
}

fn list_pids(proc_dir: &std::path::Path) -> Result<Vec<u32>, CollectError> {
    let entries = std::fs::read_dir(proc_dir)
        .map_err(|e| CollectError::Failed(format!("cannot read {}: {e}", proc_dir.display())))?;
    let mut pids: Vec<u32> = entries.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
    pids.sort_unstable();
    Ok(pids)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stat {
    pub comm: String,
    pub state: String,
    pub ppid: u32,
    pub flags: u64,
    pub cpu_ticks: u64,
    pub threads: u32,
    pub start_ticks: u64,
}

/// Parses `/proc/<pid>/stat`. The command name is parenthesised and may itself
/// contain spaces and parentheses, so fields are counted from the last `)`.
pub(crate) fn parse_stat(contents: &str) -> Option<Stat> {
    let open = contents.find('(')?;
    let close = contents.rfind(')')?;
    let comm = contents.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = contents.get(close + 1..)?.split_whitespace().collect();
    let num = |i: usize| rest.get(i)?.parse::<u64>().ok();
    Some(Stat {
        comm,
        state: rest.first()?.to_string(),
        ppid: num(1)? as u32,
        flags: num(6)?,
        cpu_ticks: num(11)? + num(12)?,
        threads: num(17)? as u32,
        start_ticks: num(19)?,
    })
}

/// Returns (effective UID, resident memory in bytes) from `/proc/<pid>/status`.
pub(crate) fn parse_status(contents: &str) -> (Option<u32>, Option<u64>) {
    let mut uid = None;
    let mut rss = None;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            uid = rest.split_whitespace().nth(1).and_then(|u| u.parse().ok());
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss = rest.split_whitespace().next().and_then(|kb| kb.parse::<u64>().ok()).map(|kb| kb * 1024);
        }
    }
    (uid, rss)
}

fn format_cmdline(ctx: &CaptureContext, raw: &[u8]) -> Option<String> {
    let args: Vec<String> = raw.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    let last = args.iter().rposition(|a| !a.is_empty())?;
    let args = &args[..=last];
    let mut line = ctx.redactor.args(args).join(" ");
    if line.len() > MAX_CMDLINE {
        let mut end = MAX_CMDLINE;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push('…');
    }
    Some(line)
}

/// `root` and all of its descendants.
pub(crate) fn subtree(list: &[Process], root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in list {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut out = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for child in children.get(&pid).into_iter().flatten() {
            if out.insert(*child) {
                stack.push(*child);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Redactor;
    use std::fs;
    use std::path::PathBuf;

    const STAT: &str = "4242 (tmux: server (x)) S 1 4242 4242 0 -1 4194560 1188 0 0 0 150 50 0 0 20 0 3 0 12345 10465280 1074 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";

    #[test]
    fn parses_stat_with_awkward_names() {
        let s = parse_stat(STAT).unwrap();
        assert_eq!(s.comm, "tmux: server (x)");
        assert_eq!(s.state, "S");
        assert_eq!(s.ppid, 1);
        assert_eq!(s.cpu_ticks, 200);
        assert_eq!(s.threads, 3);
        assert_eq!(s.start_ticks, 12345);
        assert_eq!(s.flags & PF_KTHREAD, 0);
    }

    #[test]
    fn parses_status() {
        let status = "Name:\tredis-server\nUid:\t999\t998\t998\t998\nVmRSS:\t   10240 kB\n";
        assert_eq!(parse_status(status), (Some(998), Some(10240 * 1024)));
    }

    #[test]
    fn finds_subtree() {
        let p = |pid, ppid| Process {
            pid,
            ppid,
            name: "x".into(),
            exe: None,
            cmdline: None,
            user: None,
            uid: None,
            state: "S".into(),
            cpu_percent: None,
            memory_bytes: 0,
            threads: 1,
            started_at: None,
        };
        let list = vec![p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(20, 1)];
        let mut got: Vec<u32> = subtree(&list, 10).into_iter().collect();
        got.sort();
        assert_eq!(got, [10, 11, 12]);
    }

    /// Builds a minimal /proc tree and runs the real collector against it.
    #[test]
    fn collects_from_fixture_tree() {
        let root = std::env::temp_dir().join(format!("hostprint-proc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let write = |rel: &str, contents: &[u8]| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write("proc/stat", b"cpu  1 1 1 1 0 0 0 0\nbtime 1790000000\n");
        write("etc/passwd", b"root:x:0:0::/root:/bin/sh\nredis:x:999:999::/var/lib/redis:/bin/false\n");
        // A user process with a secret on its command line.
        write("proc/100/stat", STAT.replacen("4242", "100", 1).as_bytes());
        write("proc/100/status", b"Uid:\t999\t999\t999\t999\nVmRSS:\t2048 kB\n");
        write("proc/100/cmdline", b"redis-server\0--requirepass\0hunter2\0");
        // A kernel thread (PF_KTHREAD set, no command line).
        write("proc/2/stat", b"2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 1 0 0 0");
        write("proc/2/cmdline", b"");

        let mut ctx = CaptureContext::new(Redactor::new(b"k"));
        ctx.root = PathBuf::from(&root);
        ctx.self_pid = None;
        ctx.sample_interval = std::time::Duration::from_millis(1);
        let collected = ProcessCollector.collect(&ctx).unwrap();
        let Section::Processes(procs) = collected.section else { panic!("wrong section") };
        assert_eq!(procs.kernel_threads, 1);
        assert_eq!(procs.list.len(), 1);
        let p = &procs.list[0];
        assert_eq!((p.pid, p.name.as_str(), p.user.as_deref()), (100, "tmux: server (x)", Some("redis")));
        assert_eq!(p.memory_bytes, 2048 * 1024);
        assert_eq!(p.cmdline.as_deref(), Some("redis-server --requirepass [REDACTED]"));
        assert_eq!(p.started_at.unwrap().timestamp(), 1_790_000_000 + 12345 / clock_ticks() as i64);
        let _ = fs::remove_dir_all(&root);
    }
}
