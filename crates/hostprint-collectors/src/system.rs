//! Host identity: hostname, OS, kernel, architecture, boot time.

use crate::util::read_trimmed;
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use chrono::{DateTime, Utc};
use hostprint_model::{Host, OsRelease};

pub struct SystemCollector;

impl Collector for SystemCollector {
    fn name(&self) -> &'static str {
        "system"
    }

    fn title(&self) -> &'static str {
        "System"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        #[cfg(target_os = "macos")]
        if ctx.is_live() {
            return crate::macos::live::system(ctx);
        }
        ctx.require_linux()?;
        let hostname = read_trimmed(&ctx.path("/proc/sys/kernel/hostname"))
            .or_else(|| read_trimmed(&ctx.path("/etc/hostname")))
            .ok_or_else(|| CollectError::Failed("cannot read hostname".into()))?;
        let os = std::fs::read_to_string(ctx.path("/etc/os-release"))
            .or_else(|_| std::fs::read_to_string(ctx.path("/usr/lib/os-release")))
            .ok()
            .map(|s| parse_os_release(&s));
        let kernel = read_trimmed(&ctx.path("/proc/sys/kernel/osrelease"));
        let boot_time = std::fs::read_to_string(ctx.path("/proc/stat")).ok().and_then(|s| parse_boot_time(&s));
        let uptime_seconds = read_trimmed(&ctx.path("/proc/uptime"))
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map(|secs| secs as u64);

        let host = Host {
            hostname,
            os,
            kernel,
            kernel_name: read_trimmed(&ctx.path("/proc/sys/kernel/ostype")).or_else(|| Some("Linux".into())),
            architecture: architecture(),
            boot_time,
            uptime_seconds,
            timezone: timezone(ctx),
            hardware: hardware(ctx),
            container: container_runtime(ctx),
        };
        Ok(Collected::new(Section::Host(host.clone())).summary(summary(&host)))
    }
}

/// "Ubuntu 24.04.1 LTS · Linux 6.8.0-45-generic".
pub(crate) fn summary(host: &Host) -> String {
    match (&host.os, host.kernel_display()) {
        (Some(os), Some(k)) => format!("{} · {k}", os.display()),
        (Some(os), None) => os.display(),
        _ => host.hostname.clone(),
    }
}

pub(crate) fn parse_os_release(contents: &str) -> OsRelease {
    let mut os = OsRelease { id: None, name: None, version_id: None, pretty_name: None };
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
        match key.trim() {
            "ID" => os.id = Some(value),
            "NAME" => os.name = Some(value),
            "VERSION_ID" => os.version_id = Some(value),
            "PRETTY_NAME" => os.pretty_name = Some(value),
            _ => {}
        }
    }
    os
}

pub(crate) fn parse_boot_time(proc_stat: &str) -> Option<DateTime<Utc>> {
    let secs = proc_stat.lines().find_map(|l| l.strip_prefix("btime "))?.trim().parse::<i64>().ok()?;
    DateTime::from_timestamp(secs, 0)
}

pub(crate) fn architecture() -> String {
    #[cfg(unix)]
    {
        // SAFETY: utsname is plain data; uname fills it with NUL-terminated strings.
        unsafe {
            let mut uts: libc::utsname = std::mem::zeroed();
            if libc::uname(&mut uts) == 0 {
                let machine = std::ffi::CStr::from_ptr(uts.machine.as_ptr());
                if let Ok(m) = machine.to_str() {
                    if !m.is_empty() {
                        return m.to_string();
                    }
                }
            }
        }
    }
    std::env::consts::ARCH.to_string()
}

pub(crate) fn timezone(ctx: &CaptureContext) -> Option<String> {
    if let Some(tz) = read_trimmed(&ctx.path("/etc/timezone")) {
        return Some(tz);
    }
    let target = std::fs::read_link(ctx.path("/etc/localtime")).ok()?;
    let target = target.to_string_lossy();
    target.split_once("zoneinfo/").map(|(_, zone)| zone.to_string())
}

fn hardware(ctx: &CaptureContext) -> Option<String> {
    let vendor = read_trimmed(&ctx.path("/sys/class/dmi/id/sys_vendor"));
    let product = read_trimmed(&ctx.path("/sys/class/dmi/id/product_name"));
    match (vendor, product) {
        (Some(v), Some(p)) if p.starts_with(&v) => Some(p),
        (Some(v), Some(p)) => Some(format!("{v} {p}")),
        (v, p) => v.or(p),
    }
}

fn container_runtime(ctx: &CaptureContext) -> Option<String> {
    if ctx.path("/.dockerenv").exists() {
        return Some("docker".into());
    }
    if ctx.path("/run/.containerenv").exists() {
        return Some("podman".into());
    }
    let cgroup = std::fs::read_to_string(ctx.path("/proc/1/cgroup")).unwrap_or_default();
    ["kubepods", "docker", "containerd", "lxc"].iter().find(|marker| cgroup.contains(*marker)).map(|marker| {
        if *marker == "kubepods" {
            "kubernetes".into()
        } else {
            marker.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release() {
        let os = parse_os_release(
            "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n",
        );
        assert_eq!(os.id.as_deref(), Some("ubuntu"));
        assert_eq!(os.version_id.as_deref(), Some("24.04"));
        assert_eq!(os.display(), "Ubuntu 24.04.1 LTS");
    }

    #[test]
    fn parses_boot_time() {
        let stat = "cpu  1 2 3 4\nintr 5\nbtime 1790000000\nprocesses 99\n";
        assert_eq!(parse_boot_time(stat).unwrap().timestamp(), 1_790_000_000);
        assert_eq!(parse_boot_time("cpu 1\n"), None);
    }
}
