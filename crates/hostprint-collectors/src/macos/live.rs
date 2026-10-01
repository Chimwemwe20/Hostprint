//! Live macOS collection, from `sysctl`, libc and the standard system tools
//! (`ps`, `vm_stat`, `netstat`, `lsof`, `route`, `launchctl`, `mount`). Tools
//! are run by absolute path, so a hostile or unusual `PATH` cannot change
//! what is measured.

use super::parse::*;
use crate::resources::{stat_disks, STATVFS_TIMEOUT};
use crate::util::{is_root, round1, round2, run_command, CommandOutput};
use crate::{CaptureContext, CollectError, Collected, Section};
use chrono::{DateTime, Local, TimeZone, Utc};
use hostprint_model::format;
use hostprint_model::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{CStr, CString};

fn tool(ctx: &CaptureContext, path: &str, args: &[&str]) -> Result<CommandOutput, CollectError> {
    let out =
        run_command(path, args, None, ctx.command_timeout).map_err(|e| CollectError::Failed(format!("{path}: {e}")))?;
    if out.success {
        Ok(out)
    } else {
        Err(CollectError::Failed(format!("{path}: {}", out.error_line())))
    }
}

// --- sysctl -----------------------------------------------------------------

fn sysctl_raw(name: &str) -> Option<Vec<u8>> {
    let c_name = CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    // SAFETY: the first call only reports the size; the second writes at most
    // `len` bytes into a buffer of that size.
    unsafe {
        if libc::sysctlbyname(c_name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(c_name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }
}

fn sysctl_string(name: &str) -> Option<String> {
    let raw = sysctl_raw(name)?;
    let s = String::from_utf8_lossy(&raw).trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn sysctl_u64(name: &str) -> Option<u64> {
    let raw = sysctl_raw(name)?;
    match raw.len() {
        8 => Some(u64::from_ne_bytes(raw.try_into().ok()?)),
        4 => Some(u64::from(u32::from_ne_bytes(raw.try_into().ok()?))),
        _ => None,
    }
}

fn boot_time() -> Option<DateTime<Utc>> {
    let raw = sysctl_raw("kern.boottime")?;
    if raw.len() < std::mem::size_of::<libc::timeval>() {
        return None;
    }
    // SAFETY: the buffer holds at least one timeval; read_unaligned copes with
    // the Vec's alignment.
    let tv: libc::timeval = unsafe { std::ptr::read_unaligned(raw.as_ptr().cast()) };
    DateTime::from_timestamp(tv.tv_sec, 0)
}

fn hostname() -> Option<String> {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: gethostname writes a NUL-terminated name of at most buf.len() bytes.
    unsafe {
        if libc::gethostname(buf.as_mut_ptr(), buf.len()) != 0 {
            return None;
        }
        CStr::from_ptr(buf.as_ptr()).to_str().ok().map(str::to_string)
    }
}

/// Name for a UID, from Directory Services via getpwuid_r.
fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: zeroed passwd is a valid out-parameter; getpwuid_r fills it with
    // pointers into `buf`, which outlives the read below.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        if libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) != 0 || result.is_null() {
            return None;
        }
        CStr::from_ptr(pwd.pw_name).to_str().ok().map(str::to_string)
    }
}

// --- System -----------------------------------------------------------------

pub(crate) fn system(ctx: &CaptureContext) -> Result<Collected, CollectError> {
    let hostname = hostname().ok_or_else(|| CollectError::Failed("cannot read hostname".into()))?;
    let version = sysctl_string("kern.osproductversion");
    let build = sysctl_string("kern.osversion");
    let pretty = version.as_ref().map(|v| match &build {
        Some(b) => format!("macOS {v} ({b})"),
        None => format!("macOS {v}"),
    });
    let boot_time = boot_time();
    let host = Host {
        hostname,
        os: Some(OsRelease {
            id: Some("macos".into()),
            name: Some("macOS".into()),
            version_id: version,
            pretty_name: pretty,
        }),
        kernel: sysctl_string("kern.osrelease"),
        kernel_name: Some("Darwin".into()),
        architecture: crate::system::architecture(),
        boot_time,
        uptime_seconds: boot_time.map(|b| (Utc::now() - b).num_seconds().max(0) as u64),
        timezone: crate::system::timezone(ctx),
        hardware: sysctl_string("hw.model"),
        container: None,
    };
    Ok(Collected::new(Section::Host(host.clone())).summary(crate::system::summary(&host)))
}

// --- Resources --------------------------------------------------------------

pub(crate) fn resources(ctx: &CaptureContext) -> Result<Collected, CollectError> {
    let mut notes = Vec::new();
    let total = sysctl_u64("hw.memsize").ok_or_else(|| CollectError::Failed("cannot read hw.memsize".into()))?;
    let vm = tool(ctx, "/usr/bin/vm_stat", &[])?;
    let (page, counters) =
        parse_vm_stat(&vm.stdout).ok_or_else(|| CollectError::Failed("unrecognised vm_stat output".into()))?;
    let available = available_from_vm_stat(page, &counters).unwrap_or(0).min(total);
    let memory = Memory {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total - available,
        free_bytes: counters.get("Pages free").copied().unwrap_or(0) * page,
    };
    let swap =
        match tool(ctx, "/usr/sbin/sysctl", &["-n", "vm.swapusage"]).ok().and_then(|o| parse_swapusage(&o.stdout)) {
            Some((total, used, free)) => Swap { total_bytes: total, used_bytes: used, free_bytes: free },
            None => Swap { total_bytes: 0, used_bytes: 0, free_bytes: 0 },
        };

    let mut loads = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles into the array.
    let load = (unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) } == 3).then(|| LoadAverage {
        one: round2(loads[0]),
        five: round2(loads[1]),
        fifteen: round2(loads[2]),
    });

    // `top` measures CPU over one second; this collector runs alongside the
    // others, so the capture as a whole does not wait for it twice.
    let usage = tool(ctx, "/usr/bin/top", &["-l", "2", "-n", "0", "-s", "1"])
        .ok()
        .and_then(|o| parse_top_cpu_busy(&o.stdout))
        .map(round1);
    let cpu = Cpu {
        model: sysctl_string("machdep.cpu.brand_string"),
        logical_cores: sysctl_u64("hw.logicalcpu").map(|n| n as u32).unwrap_or(1),
        physical_cores: sysctl_u64("hw.physicalcpu").map(|n| n as u32),
        usage_percent: usage,
        iowait_percent: None,
        steal_percent: None,
    };

    let disks = match tool(ctx, "/sbin/mount", &[]) {
        Ok(out) => stat_disks(ctx, relevant_mounts(parse_mount(&out.stdout))),
        Err(e) => {
            notes.push(format!("filesystems: {e}"));
            Vec::new()
        }
    };
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
    let mut collected =
        Collected::new(Section::Resources(Resources { cpu, load, memory, swap, pressure: None, disks }))
            .summary(summary);
    collected.notes = notes;
    Ok(collected)
}

// --- Processes --------------------------------------------------------------

pub(crate) fn processes(ctx: &CaptureContext) -> Result<Collected, CollectError> {
    let out = tool(ctx, "/bin/ps", &["-axo", "pid=,ppid=,uid=,state=,rss=,pcpu=,lstart=,comm="])?;
    let rows = parse_ps(&out.stdout);
    let args = tool(ctx, "/bin/ps", &["-axo", "pid=,args="]).map(|o| parse_ps_args(&o.stdout)).unwrap_or_default();
    let mut users: HashMap<u32, Option<String>> = HashMap::new();
    let mut hidden_args = 0;

    let mut list: Vec<Process> = rows
        .into_iter()
        .filter(|r| r.pid != 0) // kernel_task
        .map(|r| {
            let comm = r.comm.trim_start_matches('(').trim_end_matches(')').to_string();
            let name = comm.rsplit('/').next().unwrap_or(&comm).to_string();
            let cmdline = args.get(&r.pid).map(|a| {
                let words: Vec<String> = a.split_whitespace().map(str::to_string).collect();
                crate::util::clip(&ctx.redactor.args(&words).join(" "), 1024)
            });
            if cmdline.is_none() {
                hidden_args += 1;
            }
            let started_at =
                r.started.and_then(|n| Local.from_local_datetime(&n).earliest()).map(|t| t.with_timezone(&Utc));
            Process {
                pid: r.pid,
                ppid: r.ppid,
                exe: comm.starts_with('/').then(|| comm.clone()),
                name,
                cmdline,
                user: users.entry(r.uid).or_insert_with(|| user_name(r.uid)).clone(),
                uid: Some(r.uid),
                state: r.state,
                cpu_percent: Some(round1(r.cpu_percent)),
                memory_bytes: r.rss_bytes,
                threads: 0,
                started_at,
            }
        })
        .collect();
    if let Some(self_pid) = ctx.self_pid {
        let excluded = crate::processes::subtree(&list, self_pid);
        list.retain(|p| !excluded.contains(&p.pid));
    }
    list.sort_by_key(|p| p.pid);

    let summary = format!("{} {}", list.len(), if list.len() == 1 { "process" } else { "processes" });
    let mut collected = Collected::new(Section::Processes(Processes { list, kernel_threads: 0 })).summary(summary);
    if hidden_args > 0 && !is_root() {
        collected = collected.note(format!(
            "command lines unavailable for {hidden_args} processes owned by other users (run as root for full details)"
        ));
    }
    Ok(collected)
}

// --- Network ----------------------------------------------------------------

pub(crate) fn network(ctx: &CaptureContext) -> Result<Collected, CollectError> {
    let mut rows = parse_netstat(&tool(ctx, "/usr/sbin/netstat", &["-an", "-p", "tcp"])?.stdout);
    rows.extend(parse_netstat(&tool(ctx, "/usr/sbin/netstat", &["-an", "-p", "udp"])?.stdout));
    let mut notes = Vec::new();
    // netstat sees every socket; lsof says who owns them, for the sockets
    // this user may inspect.
    let lsof_args = ["-nP", "-iTCP", "-sTCP:LISTEN", "-iUDP", "-F", "pcPn"];
    let owners = match run_command("/usr/sbin/lsof", &lsof_args, None, ctx.command_timeout) {
        // Exit status 1 means some sockets could not be shown (or there were
        // none); what it printed is still right.
        Ok(out) if out.success || out.code == Some(1) => parse_lsof(&out.stdout),
        Ok(out) => {
            notes.push(format!("socket owners: lsof: {}", out.error_line()));
            BTreeMap::new()
        }
        Err(e) => {
            notes.push(format!("socket owners: lsof: {e}"));
            BTreeMap::new()
        }
    };

    let mut listening = BTreeSet::new();
    let mut tcp_states: BTreeMap<String, u64> = BTreeMap::new();
    let mut unowned = 0;
    for row in &rows {
        let is_listener = match row.protocol {
            "tcp" => row.state.as_deref() == Some("LISTEN"),
            _ => !row.connected,
        };
        if !is_listener {
            if let Some(state) = &row.state {
                *tcp_states.entry(state.clone()).or_default() += 1;
            }
            continue;
        }
        let owner = owners.get(&(row.protocol.to_string(), row.port));
        if owner.is_none() {
            unowned += 1;
        }
        listening.insert(ListeningSocket {
            protocol: row.protocol.to_string(),
            address: row.address.clone(),
            port: row.port,
            pid: owner.map(|o| o.0),
            process: owner.map(|o| o.1.clone()),
        });
    }
    if unowned > 0 && !is_root() {
        notes.push(format!("owning process unknown for {unowned} listening sockets (run as root for full details)"));
    }

    let mut default_gateways = Vec::new();
    for args in [&["-n", "get", "default"][..], &["-n", "get", "-inet6", "default"][..]] {
        if let Some((gateway, interface)) =
            run_command("/sbin/route", args, None, ctx.command_timeout).ok().and_then(|o| parse_route_get(&o.stdout))
        {
            default_gateways.push(Route { gateway, interface });
        }
    }
    default_gateways.sort();
    let resolv = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let ephemeral_ports = match (sysctl_u64("net.inet.ip.portrange.first"), sysctl_u64("net.inet.ip.portrange.last")) {
        (Some(start), Some(end)) => Some(PortRange { start: start as u16, end: end as u16 }),
        _ => None,
    };
    let interfaces = interfaces();
    let listening: Vec<ListeningSocket> = listening.into_iter().collect();
    let summary = format!("{} listening sockets · {} interfaces", listening.len(), interfaces.len());
    let mut collected = Collected::new(Section::Network(Network {
        interfaces,
        listening,
        tcp_states,
        default_gateways,
        dns: crate::network::parse_resolv_conf(&resolv),
        ephemeral_ports,
    }))
    .summary(summary);
    collected.notes = notes;
    Ok(collected)
}

fn interfaces() -> Vec<Interface> {
    let mut map: BTreeMap<String, Interface> = BTreeMap::new();
    // SAFETY: getifaddrs returns a linked list we only read and then free.
    // Pointers are checked for null and cast according to sa_family; the
    // link-layer address is read within sdl_nlen + sdl_alen of sdl_data.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return Vec::new();
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            let entry = map.entry(name.clone()).or_insert_with(|| Interface {
                is_virtual: is_virtual_interface(&name),
                name,
                state: None,
                mac: None,
                mtu: None,
                addresses: Vec::new(),
            });
            let flags = ifa.ifa_flags as i32;
            let up = flags & libc::IFF_UP != 0 && flags & libc::IFF_RUNNING != 0;
            entry.state = Some(if up { "up" } else { "down" }.into());
            if ifa.ifa_addr.is_null() {
                continue;
            }
            if i32::from((*ifa.ifa_addr).sa_family) == libc::AF_LINK {
                let sdl = &*(ifa.ifa_addr as *const libc::sockaddr_dl);
                if sdl.sdl_alen == 6 {
                    let data = sdl.sdl_data.as_ptr().cast::<u8>().add(usize::from(sdl.sdl_nlen));
                    let mac = std::slice::from_raw_parts(data, 6);
                    if mac.iter().any(|b| *b != 0) {
                        entry.mac = Some(mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"));
                    }
                }
                if !ifa.ifa_data.is_null() {
                    entry.mtu = Some((*(ifa.ifa_data as *const libc::if_data)).ifi_mtu);
                }
            } else if let Some(cidr) = crate::network::ifaddr_cidr(ifa) {
                entry.addresses.push(cidr);
            }
        }
        libc::freeifaddrs(head);
    }
    map.into_values()
        .map(|mut i| {
            i.addresses.sort();
            i.addresses.dedup();
            i
        })
        .collect()
}

// --- Services (launchd) -----------------------------------------------------

pub(crate) fn services(ctx: &CaptureContext) -> Result<Collected, CollectError> {
    let out = tool(ctx, "/bin/launchctl", &["list"])?;
    let mut services: Vec<Service> = parse_launchctl_list(&out.stdout)
        .into_iter()
        .map(|job| {
            let exit = |s: i32| if s < 0 { format!("signal {}", -s) } else { format!("exit {s}") };
            let (active, sub, result) = match (job.pid, job.status) {
                (Some(_), 0) => ("active", "running", None),
                (Some(_), s) => ("active", "running", Some(exit(s))),
                (None, 0) => ("inactive", "dead", None),
                (None, s) => ("failed", "failed", Some(exit(s))),
            };
            Service {
                name: job.label,
                description: None,
                load_state: "loaded".into(),
                active_state: active.into(),
                sub_state: sub.into(),
                // launchd starts most jobs on demand; the diff treats their
                // coming and going like systemd oneshots.
                service_type: Some("launchd".into()),
                restarts: None,
                result,
                active_since: None,
                main_pid: job.pid,
            }
        })
        .collect();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    let failed = services.iter().filter(|s| s.active_state == "failed").count();
    let summary = format!("{} launchd jobs · {failed} failed", services.len());
    let mut collected = Collected::new(Section::Services(services)).summary(summary);
    if !is_root() {
        collected = collected.note("only this user's launchd jobs are listed (run as root for system daemons)");
    }
    Ok(collected)
}
