use crate::{matches_any, time, Category, Change, DiffOptions, Significance::*};
use chrono::{DateTime, Utc};
use hostprint_model::format::{bytes, percent_change, signed};
use hostprint_model::{Process, Processes};
use std::collections::{BTreeMap, BTreeSet};

const MIB: u64 = 1024 * 1024;

/// Processes younger than this at capture time are treated as transient.
const TRANSIENT_SECS: i64 = 60;

/// Interactive tools and session processes: they come and go with whoever is
/// logged in, which says nothing about the system.
const INTERACTIVE: &[&str] = &[
    "bash",
    "sh",
    "dash",
    "zsh",
    "fish",
    "ksh",
    "tcsh",
    "csh",
    "sshd",
    "sshd-session",
    "sshd-auth",
    "sudo",
    "su",
    "login",
    "agetty",
    "tmux",
    "tmux: server",
    "tmux: client",
    "screen",
    "SCREEN",
    "mosh-server",
    "less",
    "more",
    "man",
    "vi",
    "vim",
    "nvim",
    "nano",
    "emacs",
    "top",
    "htop",
    "btop",
    "watch",
    "sleep",
    "ps",
    "grep",
    "tail",
    "head",
    "cat",
    "journalctl",
    "ssh",
    "docker",
    "kubectl",
    "git",
    "hostprint",
];

pub(crate) fn compare(
    a: &Processes,
    b: &Processes,
    at_a: DateTime<Utc>,
    at_b: DateTime<Utc>,
    opts: &DiffOptions,
    out: &mut Vec<Change>,
) {
    let group = |p: &Processes| {
        let mut groups: BTreeMap<String, Vec<Process>> = BTreeMap::new();
        for proc in p.list.iter().filter(|p| !matches_any(&opts.ignore_processes, &p.name)) {
            groups.entry(proc.name.clone()).or_default().push(proc.clone());
        }
        groups
    };
    let (ga, gb) = (group(a), group(b));
    let names: BTreeSet<&String> = ga.keys().chain(gb.keys()).collect();
    let change = |sig, rule, name: &str, field: &str| {
        Change::new(sig, Category::Processes, rule, format!("processes/{name}/{field}"), name)
    };

    for name in names {
        let interactive = INTERACTIVE.contains(&name.as_str());
        match (ga.get(name), gb.get(name)) {
            (Some(pa), None) => {
                let transient = interactive || pa.iter().all(|p| young(p, at_a));
                out.push(
                    change(if transient { Info } else { Medium }, "process.disappeared", name, "running")
                        .field("running")
                        .removed(describe(pa)),
                );
            }
            (None, Some(pb)) => {
                let transient = interactive || pb.iter().all(|p| young(p, at_b));
                out.push(
                    change(if transient { Info } else { Low }, "process.appeared", name, "running")
                        .field("running")
                        .added(describe(pb)),
                );
            }
            (Some(pa), Some(pb)) => {
                let (na, nb) = (pa.len(), pb.len());
                if na != nb {
                    let sig = if interactive {
                        Info
                    } else if (nb < na && na >= 2 && nb * 2 <= na) || (nb >= 2 * na && nb - na >= 5) {
                        Low
                    } else {
                        Info
                    };
                    out.push(
                        change(sig, "process.count", name, "count")
                            .field("instances")
                            .values(na.to_string(), nb.to_string())
                            .delta(signed(nb as i64 - na as i64)),
                    );
                }
                let mem = |ps: &[Process]| ps.iter().map(|p| p.memory_bytes).sum::<u64>();
                let (ma, mb) = (mem(pa), mem(pb));
                if mb > ma {
                    let grew = mb - ma;
                    let sig = if grew >= 256 * MIB && mb >= 2 * ma {
                        Some(Medium)
                    } else if grew >= 128 * MIB && 2 * mb >= 3 * ma {
                        Some(Low)
                    } else {
                        None
                    };
                    if let Some(sig) = sig {
                        out.push(
                            change(sig, "process.memory", name, "memory")
                                .field("memory (RSS)")
                                .values(bytes(ma), bytes(mb))
                                .delta(percent_change(ma as f64, mb as f64).unwrap_or_default()),
                        );
                    }
                }
                if let ([x], [y]) = (pa.as_slice(), pb.as_slice()) {
                    if let (Some(sx), Some(sy)) = (x.started_at, y.started_at) {
                        if sx != sy && !young(x, at_a) && !interactive {
                            out.push(change(Low, "process.restarted", name, "started").field("started").values(
                                format!("{} (pid {})", time(sx), x.pid),
                                format!("{} (pid {})", time(sy), y.pid),
                            ));
                        }
                    }
                    if let (Some(ex), Some(ey)) = (&x.exe, &y.exe) {
                        if ex != ey {
                            out.push(change(Low, "process.exe", name, "exe").field("executable").values(ex, ey));
                        }
                    }
                }
            }
            (None, None) => unreachable!(),
        }
    }

    let count_state = |p: &Processes, state: &str| p.list.iter().filter(|p| p.state == state).count();
    let (za, zb) = (count_state(a, "Z"), count_state(b, "Z"));
    if zb >= 5 && zb > za {
        out.push(
            Change::new(Low, Category::Processes, "process.zombies", "processes/@zombies", "Zombie processes")
                .values(za.to_string(), zb.to_string()),
        );
    }
    let (da, db) = (count_state(a, "D"), count_state(b, "D"));
    if db >= 5 && da < 5 {
        out.push(
            Change::new(
                Medium,
                Category::Processes,
                "process.uninterruptible",
                "processes/@uninterruptible",
                "Processes blocked in uninterruptible sleep (D)",
            )
            .values(da.to_string(), db.to_string()),
        );
    }
    let (ta, tb) = (a.list.len(), b.list.len());
    if ta.abs_diff(tb) >= 10.max(ta / 10) {
        out.push(
            Change::new(Info, Category::Processes, "process.total", "processes/@total", "Process count")
                .values(ta.to_string(), tb.to_string())
                .delta(signed(tb as i64 - ta as i64)),
        );
    }
}

fn young(p: &Process, captured_at: DateTime<Utc>) -> bool {
    p.started_at.is_some_and(|s| (captured_at - s).num_seconds() < TRANSIENT_SECS)
}

fn describe(procs: &[Process]) -> String {
    let memory = bytes(procs.iter().map(|p| p.memory_bytes).sum());
    match procs {
        [p] => format!("pid {}, {memory}", p.pid),
        _ => format!("{} processes, {memory}", procs.len()),
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};
    use hostprint_model::Snapshot;

    fn changes_with(opts: DiffOptions, edit: impl FnOnce(&mut Snapshot)) -> Vec<Change> {
        let a = baseline();
        let mut b = later(&a, 600);
        edit(&mut b);
        diff(&a, &b, &opts).changes
    }

    fn rules(changes: &[Change]) -> Vec<(&str, Significance)> {
        changes.iter().map(|c| (c.rule.as_str(), c.significance)).collect()
    }

    fn list(s: &mut Snapshot) -> &mut Vec<hostprint_model::Process> {
        &mut s.processes.as_mut().unwrap().list
    }

    #[test]
    fn pid_changes_alone_are_not_changes() {
        let c = changes_with(DiffOptions::default(), |b| {
            for p in list(b) {
                p.pid += 1000;
                p.cpu_percent = Some(50.0);
            }
        });
        assert_eq!(c, []);
    }

    #[test]
    fn disappeared_daemon_is_medium_and_new_daemon_low() {
        let c = changes_with(DiffOptions::default(), |b| {
            list(b).retain(|p| p.name != "redis-server");
            list(b).push(process(900, "worker", 50 * MIB, at(0)));
        });
        assert_eq!(rules(&c), [("process.disappeared", Medium), ("process.appeared", Low)]);
        assert_eq!(c[0].before.as_deref(), Some("pid 600, 20.0 MiB"));
    }

    #[test]
    fn transient_and_interactive_processes_are_info() {
        let c = changes_with(DiffOptions::default(), |b| {
            list(b).push(process(901, "cron-job", MIB, at(590))); // 10s old at capture
            list(b).push(process(902, "bash", MIB, at(0)));
            list(b).retain(|p| p.name != "sshd");
        });
        assert!(c.iter().all(|c| c.significance == Info), "{c:#?}");
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn memory_growth_and_restarts() {
        let c = changes_with(DiffOptions::default(), |b| {
            let node = list(b).iter_mut().find(|p| p.name == "node").unwrap();
            node.memory_bytes = 900 * MIB;
            node.started_at = Some(at(300));
            node.pid = 4410;
        });
        assert_eq!(rules(&c), [("process.memory", Medium), ("process.restarted", Low)]);
        assert_eq!(c[0].delta.as_deref(), Some("+200%"));
    }

    #[test]
    fn ignored_processes_are_skipped() {
        let opts = DiffOptions { ignore_processes: vec!["redis*".into()], ..Default::default() };
        let c = changes_with(opts, |b| list(b).retain(|p| p.name != "redis-server"));
        assert_eq!(c, []);
    }

    #[test]
    fn uninterruptible_processes() {
        let c = changes_with(DiffOptions::default(), |b| {
            for (i, p) in list(b).iter_mut().enumerate() {
                if i < 5 {
                    p.state = "D".into();
                }
            }
        });
        assert_eq!(rules(&c), [("process.uninterruptible", Medium)]);
    }
}
