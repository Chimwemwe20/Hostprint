//! Parsers for the output of macOS system tools. Pure functions, compiled and
//! tested on every platform; `live.rs` runs the tools.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::resources::Mount;
use chrono::NaiveDateTime;
use std::collections::BTreeMap;

/// `vm_stat`: (page size, counters by name).
pub(crate) fn parse_vm_stat(out: &str) -> Option<(u64, BTreeMap<String, u64>)> {
    let mut lines = out.lines();
    let header = lines.next()?;
    let page_size: u64 = header.split("page size of ").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let counters = lines
        .filter_map(|l| {
            let (key, value) = l.rsplit_once(':')?;
            let n = value.trim().trim_end_matches('.').parse().ok()?;
            Some((key.trim().trim_matches('"').to_string(), n))
        })
        .collect();
    Some((page_size, counters))
}

/// Available memory from `vm_stat`, in bytes: free, inactive and
/// speculative pages, which macOS hands out without swapping.
pub(crate) fn available_from_vm_stat(page_size: u64, c: &BTreeMap<String, u64>) -> Option<u64> {
    let get = |k: &str| c.get(k).copied();
    Some((get("Pages free")? + get("Pages inactive").unwrap_or(0) + get("Pages speculative").unwrap_or(0)) * page_size)
}

/// `sysctl -n vm.swapusage`: "total = 2048.00M  used = 1059.25M  free = 988.75M  (encrypted)".
/// Returns (total, used, free) in bytes.
pub(crate) fn parse_swapusage(out: &str) -> Option<(u64, u64, u64)> {
    let value = |key: &str| -> Option<u64> {
        let rest = out.split(&format!("{key} = ")).nth(1)?;
        let token = rest.split_whitespace().next()?;
        let (number, unit) = token.split_at(token.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(token.len()));
        let n: f64 = number.parse().ok()?;
        let scale = match unit {
            "" | "B" => 1.0,
            "K" => 1024.0,
            "M" => 1024.0 * 1024.0,
            "G" => 1024.0 * 1024.0 * 1024.0,
            "T" => 1024.0_f64.powi(4),
            _ => return None,
        };
        Some((n * scale) as u64)
    };
    Some((value("total")?, value("used")?, value("free")?))
}

/// `top -l 2 -n 0 -s 1`: busy percentage from the last "CPU usage" line.
pub(crate) fn parse_top_cpu_busy(out: &str) -> Option<f64> {
    let line = out.lines().rev().find(|l| l.trim_start().starts_with("CPU usage:"))?;
    let idle = line.split(',').find(|p| p.contains("idle"))?;
    let pct: f64 = idle.trim().split('%').next()?.trim().parse().ok()?;
    Some((100.0 - pct).clamp(0.0, 100.0))
}

/// `mount`: "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse)".
pub(crate) fn parse_mount(out: &str) -> Vec<Mount> {
    out.lines()
        .filter_map(|line| {
            let (device, rest) = line.split_once(" on ")?;
            let open = rest.rfind(" (")?;
            let mount_point = rest[..open].to_string();
            let options: Vec<&str> = rest[open + 2..].trim_end_matches(')').split(',').map(str::trim).collect();
            Some(Mount {
                device: device.to_string(),
                mount_point,
                filesystem: options.first()?.to_string(),
                read_only: options.iter().any(|o| *o == "read-only" || *o == "rdonly"),
            })
        })
        .collect()
}

/// Mounts that hold data. The sealed system volume `/` and the data volume
/// stay; APFS helper volumes, devfs and automounts go.
pub(crate) fn relevant_mounts(mounts: Vec<Mount>) -> Vec<Mount> {
    let mut out: Vec<Mount> = mounts
        .into_iter()
        .filter(|m| !matches!(m.filesystem.as_str(), "devfs" | "autofs" | "nullfs" | "fdesc"))
        .filter(|m| {
            m.mount_point == "/"
                || m.mount_point == "/System/Volumes/Data"
                || !(m.mount_point.starts_with("/System/Volumes/") || m.mount_point.starts_with("/private/var/vm"))
        })
        .collect();
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out.dedup_by(|a, b| a.mount_point == b.mount_point);
    out
}

/// One row of `ps -axo pid=,ppid=,uid=,state=,rss=,pcpu=,lstart=,comm=`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PsRow {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    /// Normalised to the Linux letters the diff rules use (U → D).
    pub state: String,
    pub rss_bytes: u64,
    pub cpu_percent: f64,
    /// Local time, as printed.
    pub started: Option<NaiveDateTime>,
    /// Full path of the executable (macOS `comm`).
    pub comm: String,
}

pub(crate) fn parse_ps(out: &str) -> Vec<PsRow> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 12 {
                return None;
            }
            // lstart is "Thu Oct  1 14:22:39 2026"; the weekday is redundant.
            let started = NaiveDateTime::parse_from_str(&f[7..11].join(" "), "%b %d %H:%M:%S %Y").ok();
            Some(PsRow {
                pid: f[0].parse().ok()?,
                ppid: f[1].parse().ok()?,
                uid: f[2].parse().ok()?,
                state: macos_state(f[3]).to_string(),
                rss_bytes: f[4].parse::<u64>().ok()? * 1024,
                cpu_percent: f[5].parse().ok()?,
                started,
                comm: f[11..].join(" "),
            })
        })
        .collect()
}

/// macOS process states: R running, S sleeping, I idle (asleep > 20 s),
/// U uninterruptible wait, T stopped, Z zombie.
fn macos_state(s: &str) -> &'static str {
    match s.chars().next() {
        Some('R') => "R",
        Some('U') => "D",
        Some('T') => "T",
        Some('Z') => "Z",
        _ => "S",
    }
}

/// `ps -axo pid=,args=`: pid → arguments. Processes whose arguments are not
/// readable (other users') print `(name)`; those are left out.
pub(crate) fn parse_ps_args(out: &str) -> BTreeMap<u32, String> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, args) = line.split_once(char::is_whitespace)?;
            let args = args.trim();
            if args.is_empty() || (args.starts_with('(') && args.ends_with(')')) {
                return None;
            }
            Some((pid.parse().ok()?, args.to_string()))
        })
        .collect()
}

/// A socket from `netstat -an -p tcp|udp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetstatRow {
    pub protocol: &'static str,
    pub address: String,
    pub port: u16,
    /// TCP state in Linux spelling, `None` for UDP.
    pub state: Option<String>,
    pub connected: bool,
}

pub(crate) fn parse_netstat(out: &str) -> Vec<NetstatRow> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let proto = *f.first()?;
            let protocol = if proto.starts_with("tcp") {
                "tcp"
            } else if proto.starts_with("udp") {
                "udp"
            } else {
                return None;
            };
            let (local, foreign) = (*f.get(3)?, *f.get(4)?);
            let (addr, port) = local.rsplit_once('.')?;
            let port: u16 = port.parse().ok()?;
            let address = match addr {
                "*" if proto.ends_with('4') => "0.0.0.0".to_string(),
                "*" => "::".to_string(),
                a => a.split('%').next().unwrap_or(a).to_string(),
            };
            let state = (protocol == "tcp").then(|| f.get(5).map(|s| linux_tcp_state(s).to_string())).flatten();
            Some(NetstatRow { protocol, address, port, state, connected: foreign != "*.*" })
        })
        .collect()
}

/// BSD spells some TCP states differently; the diff rules use Linux names.
fn linux_tcp_state(s: &str) -> &str {
    match s {
        "SYN_RCVD" => "SYN_RECV",
        "FIN_WAIT_1" => "FIN_WAIT1",
        "FIN_WAIT_2" => "FIN_WAIT2",
        other => other,
    }
}

/// `lsof -nP -iTCP -sTCP:LISTEN -iUDP -F pcPn`: (protocol, port) → (pid, command).
pub(crate) fn parse_lsof(out: &str) -> BTreeMap<(String, u16), (u32, String)> {
    let mut owners = BTreeMap::new();
    let (mut pid, mut command, mut protocol) = (0u32, String::new(), String::new());
    for line in out.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = value.parse().unwrap_or(0),
            "c" => command = value.to_string(),
            "P" => protocol = value.to_ascii_lowercase(),
            "n" if !value.contains("->") => {
                if let Some(port) = value.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()) {
                    owners.entry((protocol.clone(), port)).or_insert_with(|| (pid, command.clone()));
                }
            }
            _ => {}
        }
    }
    owners
}

/// `route -n get default`: (gateway, interface).
pub(crate) fn parse_route_get(out: &str) -> Option<(String, String)> {
    let field = |name: &str| {
        out.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
    };
    Some((field("gateway:")?, field("interface:")?))
}

/// A job from `launchctl list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchdJob {
    pub label: String,
    pub pid: Option<u32>,
    /// Last exit status; negative means killed by that signal.
    pub status: i32,
}

pub(crate) fn parse_launchctl_list(out: &str) -> Vec<LaunchdJob> {
    out.lines()
        .skip_while(|l| l.starts_with("PID"))
        .filter_map(|line| {
            let mut f = line.split('\t');
            let pid = f.next()?.trim();
            let status = f.next()?.trim().parse().ok()?;
            let label = f.next()?.trim().to_string();
            (!label.is_empty()).then(|| LaunchdJob { label, pid: pid.parse().ok(), status })
        })
        .collect()
}

/// Interface names macOS uses for software interfaces.
pub(crate) fn is_virtual_interface(name: &str) -> bool {
    const PREFIXES: &[&str] =
        &["lo", "utun", "bridge", "awdl", "llw", "gif", "stf", "anpi", "ap", "vmenet", "vnic", "ipsec", "ppp", "feth"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vm_stat() {
        let out = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                   Pages free:                               12000.\n\
                   Pages active:                            234567.\n\
                   Pages inactive:                           20000.\n\
                   Pages speculative:                         1000.\n\
                   \"Translation faults\":                 123456789.\n";
        let (page, c) = parse_vm_stat(out).unwrap();
        assert_eq!(page, 16384);
        assert_eq!(c["Translation faults"], 123_456_789);
        assert_eq!(available_from_vm_stat(page, &c), Some(33_000 * 16384));
    }

    #[test]
    fn parses_swapusage_and_top() {
        assert_eq!(
            parse_swapusage("total = 2048.00M  used = 1024.50M  free = 1023.50M  (encrypted)"),
            Some((2048 << 20, 1_074_266_112, 1_073_217_536))
        );
        assert_eq!(parse_swapusage("total = 0.00M  used = 0.00M  free = 0.00M"), Some((0, 0, 0)));
        let top = "Processes: 512 total\nCPU usage: 3.10% user, 2.90% sys, 94.0% idle \nPhysMem: 15G used\n\
                   Processes: 513 total\nCPU usage: 12.5% user, 7.5% sys, 80.0% idle \n";
        assert_eq!(parse_top_cpu_busy(top), Some(20.0));
    }

    #[test]
    fn parses_and_filters_mounts() {
        let out = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
                   devfs on /dev (devfs, local, nobrowse)\n\
                   /dev/disk3s6 on /System/Volumes/VM (apfs, local, noexec, journaled, noatime, nobrowse)\n\
                   /dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse, protect, root data)\n\
                   map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)\n\
                   /dev/disk4s1 on /Volumes/My Drive (msdos, local, nodev, nosuid, noowners)\n";
        let mounts = relevant_mounts(parse_mount(out));
        let points: Vec<&str> = mounts.iter().map(|m| m.mount_point.as_str()).collect();
        assert_eq!(points, ["/", "/System/Volumes/Data", "/Volumes/My Drive"]);
        assert!(mounts[0].read_only);
        assert_eq!((mounts[2].filesystem.as_str(), mounts[2].read_only), ("msdos", false));
    }

    #[test]
    fn parses_ps() {
        let out = "    1     0     0 Ss       12345   0.1 Thu Oct  1 09:00:01 2026     /sbin/launchd\n\
                   812     1   501 U         2048  12.5 Mon Sep 28 14:22:39 2026     /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n\
                   bogus line\n";
        let rows = parse_ps(out);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].pid, rows[0].ppid, rows[0].uid, rows[0].state.as_str()), (1, 0, 0, "S"));
        assert_eq!(rows[0].rss_bytes, 12345 * 1024);
        assert_eq!(rows[1].state, "D", "U is uninterruptible");
        assert_eq!(rows[1].comm, "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        assert_eq!(rows[0].started.unwrap().to_string(), "2026-10-01 09:00:01", "single-digit day");
        assert_eq!(rows[1].started.unwrap().to_string(), "2026-09-28 14:22:39");
        let args = parse_ps_args("    1 /sbin/launchd\n  812 /usr/bin/ssh -i key host\n  900 (secd)\n");
        assert_eq!(args.get(&812).map(String::as_str), Some("/usr/bin/ssh -i key host"));
        assert!(!args.contains_key(&900), "unreadable arguments are left out");
    }

    #[test]
    fn parses_netstat() {
        let tcp = "Active Internet connections (including servers)\n\
                   Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)\n\
                   tcp4       0      0  192.168.1.5.50544      17.57.146.135.443      ESTABLISHED\n\
                   tcp6       0      0  *.5000                 *.*                    LISTEN\n\
                   tcp4       0      0  127.0.0.1.5432         *.*                    LISTEN\n\
                   tcp4       0      0  *.22                   *.*                    LISTEN\n\
                   tcp6       0      0  fe80::1%lo0.631        *.*                    LISTEN\n\
                   tcp4       0      0  10.0.0.2.8080          10.0.0.9.51000         SYN_RCVD\n";
        let rows = parse_netstat(tcp);
        assert_eq!(rows.len(), 6);
        assert_eq!((rows[1].address.as_str(), rows[1].port), ("::", 5000));
        assert_eq!((rows[3].address.as_str(), rows[3].port), ("0.0.0.0", 22));
        assert_eq!(rows[4].address, "fe80::1");
        assert_eq!(rows[5].state.as_deref(), Some("SYN_RECV"));
        assert!(rows[0].connected && !rows[1].connected);
        let udp = "udp4       0      0  *.5353                 *.*\nudp4  0  0  192.168.1.5.123  *.*\n";
        let rows = parse_netstat(udp);
        assert_eq!((rows[0].protocol, rows[0].state.clone()), ("udp", None));
    }

    #[test]
    fn parses_lsof_owners() {
        let out = "p123\ncpostgres\nf7\nPTCP\nn[::1]:5432\nf8\nPTCP\nn127.0.0.1:5432\n\
                   p456\ncmDNSResponder\nf3\nPUDP\nn*:5353\nf4\nPUDP\nn*:*\nf5\nPUDP\nn10.0.0.5:5353->224.0.0.251:5353\n";
        let owners = parse_lsof(out);
        assert_eq!(owners.get(&("tcp".to_string(), 5432)), Some(&(123, "postgres".to_string())));
        assert_eq!(owners.get(&("udp".to_string(), 5353)), Some(&(456, "mDNSResponder".to_string())));
        assert_eq!(owners.len(), 2);
    }

    #[test]
    fn parses_route_and_launchctl() {
        let route = "   route to: default\ndestination: default\n    gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(parse_route_get(route), Some(("192.168.1.1".into(), "en0".into())));
        assert_eq!(parse_route_get("route: writing to routing socket: not in table\n"), None);
        let jobs = parse_launchctl_list(
            "PID\tStatus\tLabel\n-\t0\tcom.apple.SafariHistoryServiceAgent\n1234\t0\tcom.apple.Finder\n\
             -\t-9\tcom.example.crashy\n567\t1\thomebrew.mxcl.postgresql@16\n",
        );
        assert_eq!(jobs.len(), 4);
        assert_eq!(jobs[1], LaunchdJob { label: "com.apple.Finder".into(), pid: Some(1234), status: 0 });
        assert_eq!((jobs[2].pid, jobs[2].status), (None, -9));
        assert!(is_virtual_interface("utun3") && is_virtual_interface("lo0") && !is_virtual_interface("en0"));
    }
}
