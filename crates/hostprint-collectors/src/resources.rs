//! CPU, memory, swap, pressure and disk usage.

use crate::util::{read_trimmed, round1, round2};
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::format;
use hostprint_model::{Cpu, Disk, LoadAverage, Memory, Pressure, Resources, Swap};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a filesystem may take to answer `statvfs` before it is reported
/// as unresponsive. A hung NFS mount must not hang the capture.
const STATVFS_TIMEOUT: Duration = Duration::from_secs(2);

/// Filesystems that never hold user data, or that churn without meaning
/// (snap squashfs images are always 100% full).
const PSEUDO_FILESYSTEMS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "fuse.gvfsd-fuse",
    "fuse.portal",
    "fuse.snapfuse",
    "hugetlbfs",
    "mqueue",
    "nfsd",
    "nsfs",
    "proc",
    "pstore",
    "ramfs",
    "rpc_pipefs",
    "securityfs",
    "selinuxfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// Mount points whose contents belong to the container or package runtime.
const IGNORED_MOUNT_PREFIXES: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/snap",
    "/var/lib/docker",
    "/var/lib/containers",
    "/var/lib/kubelet",
    "/var/snap",
];

pub struct ResourcesCollector;

impl Collector for ResourcesCollector {
    fn name(&self) -> &'static str {
        "resources"
    }

    fn title(&self) -> &'static str {
        "Resources"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        ctx.require_linux()?;
        let stat_path = ctx.path("/proc/stat");
        let first = std::fs::read_to_string(&stat_path).ok().and_then(|s| parse_cpu_times(&s));
        // Disks are probed while the CPU sample window elapses.
        let disks = collect_disks(ctx);
        let elapsed = Instant::now();
        if first.is_some() {
            std::thread::sleep(ctx.sample_interval.saturating_sub(elapsed.elapsed()));
        }
        let second = std::fs::read_to_string(&stat_path).ok().and_then(|s| parse_cpu_times(&s));

        let cpuinfo = std::fs::read_to_string(ctx.path("/proc/cpuinfo")).unwrap_or_default();
        let (model, logical, physical) = parse_cpuinfo(&cpuinfo);
        let mut cpu = Cpu {
            model,
            logical_cores: if logical > 0 { logical } else { online_cpus() },
            physical_cores: physical,
            usage_percent: None,
            iowait_percent: None,
            steal_percent: None,
        };
        if let (Some(a), Some(b)) = (first, second) {
            let total = b.total.saturating_sub(a.total) as f64;
            if total > 0.0 {
                let idle = b.idle.saturating_sub(a.idle) as f64;
                cpu.usage_percent = Some(round1((total - idle) / total * 100.0));
                cpu.iowait_percent = Some(round1(b.iowait.saturating_sub(a.iowait) as f64 / total * 100.0));
                cpu.steal_percent = Some(round1(b.steal.saturating_sub(a.steal) as f64 / total * 100.0));
            }
        }

        let meminfo = std::fs::read_to_string(ctx.path("/proc/meminfo"))
            .map_err(|e| CollectError::Failed(format!("cannot read /proc/meminfo: {e}")))?;
        let (memory, swap) = parse_meminfo(&meminfo);
        let load = read_trimmed(&ctx.path("/proc/loadavg")).and_then(|s| parse_loadavg(&s));
        let pressure = read_pressure(ctx);

        let mut notes = Vec::new();
        for disk in disks.iter().filter(|d| d.unresponsive) {
            notes.push(format!("{} did not respond within {}s", disk.mount_point, STATVFS_TIMEOUT.as_secs()));
        }
        let summary = format!(
            "{} cores · {} memory · {} {}",
            cpu.logical_cores,
            format::bytes(memory.total_bytes),
            disks.len(),
            if disks.len() == 1 { "filesystem" } else { "filesystems" }
        );
        let resources = Resources { cpu, load, memory, swap, pressure, disks };
        let mut collected = Collected::new(Section::Resources(resources)).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CpuTimes {
    pub total: u64,
    /// idle + iowait
    pub idle: u64,
    pub iowait: u64,
    pub steal: u64,
}

pub(crate) fn parse_cpu_times(proc_stat: &str) -> Option<CpuTimes> {
    let line = proc_stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    let get = |i: usize| v.get(i).copied().unwrap_or(0);
    // user nice system idle iowait irq softirq steal; guest time is already
    // included in user and nice.
    let total = (0..8).map(get).sum();
    Some(CpuTimes { total, idle: get(3) + get(4), iowait: get(4), steal: get(7) })
}

/// Returns (model, logical cores, physical cores).
pub(crate) fn parse_cpuinfo(cpuinfo: &str) -> (Option<String>, u32, Option<u32>) {
    let mut model = None;
    let mut logical = 0;
    let mut cores = HashSet::new();
    let mut physical_id = None;
    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "processor" => logical += 1,
            "model name" | "Model" | "Hardware" if model.is_none() && !value.is_empty() => {
                model = Some(value.to_string());
            }
            "physical id" => physical_id = Some(value.to_string()),
            "core id" => {
                cores.insert((physical_id.clone(), value.to_string()));
            }
            _ => {}
        }
    }
    let physical = (!cores.is_empty()).then_some(cores.len() as u32);
    (model, logical, physical)
}

fn online_cpus() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}

pub(crate) fn parse_meminfo(meminfo: &str) -> (Memory, Swap) {
    let fields: HashMap<&str, u64> = meminfo
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some((key.trim(), kb * 1024))
        })
        .collect();
    let get = |k: &str| fields.get(k).copied().unwrap_or(0);
    let total = get("MemTotal");
    let free = get("MemFree");
    // MemAvailable exists since Linux 3.14; approximate it on older kernels.
    let available =
        fields.get("MemAvailable").copied().unwrap_or_else(|| free + get("Buffers") + get("Cached")).min(total);
    let swap_total = get("SwapTotal");
    let swap_free = get("SwapFree").min(swap_total);
    (
        Memory { total_bytes: total, available_bytes: available, used_bytes: total - available, free_bytes: free },
        Swap { total_bytes: swap_total, used_bytes: swap_total - swap_free, free_bytes: swap_free },
    )
}

pub(crate) fn parse_loadavg(loadavg: &str) -> Option<LoadAverage> {
    let mut it = loadavg.split_whitespace().map(|f| f.parse::<f64>().ok());
    Some(LoadAverage { one: round2(it.next()??), five: round2(it.next()??), fifteen: round2(it.next()??) })
}

/// Returns the `avg60` values of the `some` and `full` lines of a PSI file.
pub(crate) fn parse_pressure(contents: &str) -> (Option<f64>, Option<f64>) {
    let avg60 = |prefix: &str| {
        contents
            .lines()
            .find(|l| l.starts_with(prefix))?
            .split_whitespace()
            .find_map(|f| f.strip_prefix("avg60="))?
            .parse::<f64>()
            .ok()
            .map(round2)
    };
    (avg60("some "), avg60("full "))
}

fn read_pressure(ctx: &CaptureContext) -> Option<Pressure> {
    let read = |name: &str| std::fs::read_to_string(ctx.path(&format!("/proc/pressure/{name}"))).ok();
    let cpu = read("cpu").map(|s| parse_pressure(&s));
    let memory = read("memory").map(|s| parse_pressure(&s));
    let io = read("io").map(|s| parse_pressure(&s));
    if cpu.is_none() && memory.is_none() && io.is_none() {
        return None;
    }
    Some(Pressure {
        cpu_some_avg60: cpu.and_then(|p| p.0),
        memory_some_avg60: memory.and_then(|p| p.0),
        memory_full_avg60: memory.and_then(|p| p.1),
        io_some_avg60: io.and_then(|p| p.0),
        io_full_avg60: io.and_then(|p| p.1),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mount {
    pub device: String,
    pub mount_point: String,
    pub filesystem: String,
    pub read_only: bool,
}

pub(crate) fn parse_mounts(contents: &str) -> Vec<Mount> {
    contents
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let device = unescape_mount(f.next()?);
            let mount_point = unescape_mount(f.next()?);
            let filesystem = f.next()?.to_string();
            let read_only = f.next()?.split(',').any(|o| o == "ro");
            Some(Mount { device, mount_point, filesystem, read_only })
        })
        .collect()
}

/// `/proc/mounts` escapes space, tab, newline and backslash as octal.
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
            let code = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Keeps filesystems that hold data, one entry per device.
pub(crate) fn relevant_mounts(mounts: Vec<Mount>, is_dir: impl Fn(&str) -> bool) -> Vec<Mount> {
    let mut by_device: HashMap<String, Mount> = HashMap::new();
    for m in mounts {
        if PSEUDO_FILESYSTEMS.contains(&m.filesystem.as_str()) {
            continue;
        }
        if m.filesystem == "overlay" && m.mount_point != "/" {
            continue;
        }
        let under_ignored =
            IGNORED_MOUNT_PREFIXES.iter().any(|p| m.mount_point == *p || m.mount_point.starts_with(&format!("{p}/")));
        // Containers bind-mount single files such as /etc/hosts.
        if under_ignored || !is_dir(&m.mount_point) {
            continue;
        }
        match by_device.get(&m.device) {
            Some(existing) if existing.mount_point.len() <= m.mount_point.len() => {}
            _ => {
                by_device.insert(m.device.clone(), m);
            }
        }
    }
    let mut out: Vec<Mount> = by_device.into_values().collect();
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out
}

fn collect_disks(ctx: &CaptureContext) -> Vec<Disk> {
    let Ok(contents) = std::fs::read_to_string(ctx.path("/proc/mounts")) else {
        return Vec::new();
    };
    let mounts = relevant_mounts(parse_mounts(&contents), |mp| ctx.path(mp).is_dir());

    // Probe every filesystem concurrently, under one shared deadline.
    let pending: Vec<(Mount, mpsc::Receiver<Option<FsStats>>)> = mounts
        .into_iter()
        .map(|m| {
            let (tx, rx) = mpsc::channel();
            let path = ctx.path(&m.mount_point);
            std::thread::spawn(move || {
                let _ = tx.send(statvfs(&path));
            });
            (m, rx)
        })
        .collect();
    let deadline = Instant::now() + STATVFS_TIMEOUT;
    pending
        .into_iter()
        .map(|(m, rx)| {
            let result = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            let mut disk = Disk {
                mount_point: m.mount_point,
                device: m.device,
                filesystem: m.filesystem,
                read_only: m.read_only,
                unresponsive: false,
                total_bytes: None,
                used_bytes: None,
                available_bytes: None,
                inodes_total: None,
                inodes_free: None,
            };
            match result {
                Ok(Some(st)) => {
                    disk.total_bytes = Some(st.total);
                    disk.used_bytes = Some(st.total.saturating_sub(st.free));
                    disk.available_bytes = Some(st.avail);
                    if st.files > 0 {
                        disk.inodes_total = Some(st.files);
                        disk.inodes_free = Some(st.ffree);
                    }
                }
                Ok(None) => {}
                Err(_) => disk.unresponsive = true,
            }
            disk
        })
        .collect()
}

struct FsStats {
    total: u64,
    free: u64,
    avail: u64,
    files: u64,
    ffree: u64,
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // field widths differ between platforms
fn statvfs(path: &Path) -> Option<FsStats> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is plain data and c_path is NUL-terminated.
    let st = unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut st) != 0 {
            return None;
        }
        st
    };
    let unit = if st.f_frsize > 0 { st.f_frsize as u64 } else { st.f_bsize as u64 };
    Some(FsStats {
        total: st.f_blocks as u64 * unit,
        free: st.f_bfree as u64 * unit,
        avail: st.f_bavail as u64 * unit,
        files: st.f_files as u64,
        ffree: st.f_ffree as u64,
    })
}

#[cfg(not(unix))]
fn statvfs(_path: &Path) -> Option<FsStats> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_times() {
        let stat = "cpu  100 5 50 800 20 3 2 10 0 0\ncpu0 1 2 3 4\n";
        let t = parse_cpu_times(stat).unwrap();
        assert_eq!(t, CpuTimes { total: 990, idle: 820, iowait: 20, steal: 10 });
    }

    #[test]
    fn parses_cpuinfo() {
        let info = "processor\t: 0\nmodel name\t: AMD EPYC 7B13\nphysical id\t: 0\ncore id\t: 0\n\n\
                    processor\t: 1\nmodel name\t: AMD EPYC 7B13\nphysical id\t: 0\ncore id\t: 0\n\n\
                    processor\t: 2\nmodel name\t: AMD EPYC 7B13\nphysical id\t: 0\ncore id\t: 1\n";
        let (model, logical, physical) = parse_cpuinfo(info);
        assert_eq!(model.as_deref(), Some("AMD EPYC 7B13"));
        assert_eq!(logical, 3);
        assert_eq!(physical, Some(2));
    }

    #[test]
    fn parses_meminfo() {
        let info = "MemTotal:        8000000 kB\nMemFree:          500000 kB\nMemAvailable:    1800000 kB\n\
                    Buffers:          100000 kB\nCached:          1000000 kB\nSwapTotal:       2000000 kB\nSwapFree:        1500000 kB\n";
        let (mem, swap) = parse_meminfo(info);
        assert_eq!(mem.total_bytes, 8_000_000 * 1024);
        assert_eq!(mem.available_bytes, 1_800_000 * 1024);
        assert_eq!(mem.used_bytes, 6_200_000 * 1024);
        assert_eq!(swap.used_bytes, 500_000 * 1024);
        // Old kernels without MemAvailable.
        let (mem, _) = parse_meminfo("MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 250 kB\n");
        assert_eq!(mem.available_bytes, 400 * 1024);
    }

    #[test]
    fn parses_load_and_pressure() {
        let load = parse_loadavg("0.42 0.51 0.60 2/512 12345").unwrap();
        assert_eq!((load.one, load.five, load.fifteen), (0.42, 0.51, 0.6));
        let psi = "some avg10=1.50 avg60=12.25 avg300=3.00 total=123\nfull avg10=0.00 avg60=4.10 avg300=0.00 total=9\n";
        assert_eq!(parse_pressure(psi), (Some(12.25), Some(4.1)));
    }

    #[test]
    fn filters_and_dedups_mounts() {
        let mounts = parse_mounts(
            "/dev/sda1 / ext4 rw,relatime 0 0\n\
             proc /proc proc rw 0 0\n\
             tmpfs /run tmpfs rw 0 0\n\
             /dev/loop3 /snap/core/123 squashfs ro 0 0\n\
             /dev/sdb1 /data xfs ro,noatime 0 0\n\
             /dev/sdb1 /data/bind xfs ro,noatime 0 0\n\
             /dev/sda1 /etc/hosts ext4 rw 0 0\n\
             overlay /var/lib/docker/overlay2/abc/merged overlay rw 0 0\n\
             /dev/sdc1 /mnt/my\\040drive ext4 rw 0 0\n",
        );
        let kept = relevant_mounts(mounts, |mp| mp != "/etc/hosts");
        let points: Vec<&str> = kept.iter().map(|m| m.mount_point.as_str()).collect();
        assert_eq!(points, ["/", "/data", "/mnt/my drive"]);
        assert!(kept[1].read_only);
        assert!(!kept[0].read_only);
    }
}
