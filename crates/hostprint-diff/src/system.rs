use crate::{major_version, opt_or, time, Category, Change, Significance::*};
use hostprint_model::Host;

/// Boot times within this many seconds are the same boot (clock adjustments).
const BOOT_TOLERANCE_SECS: i64 = 60;

pub(crate) fn compare(a: &Host, b: &Host, out: &mut Vec<Change>) {
    let change = |sig, rule, key: &str, subject: &str| {
        Change::new(sig, Category::System, rule, format!("system/{key}"), subject)
    };

    if a.hostname != b.hostname {
        out.push(change(Medium, "system.hostname", "hostname", "Hostname").values(&a.hostname, &b.hostname));
    }
    let os = |h: &Host| h.os.as_ref().map(|o| o.display());
    if os(a) != os(b) {
        out.push(
            change(Medium, "system.os", "os", "Operating system")
                .values(os(a).unwrap_or_else(|| "unknown".into()), os(b).unwrap_or_else(|| "unknown".into())),
        );
    }
    if a.kernel != b.kernel {
        let (ka, kb) = (opt_or(a.kernel.as_deref(), "unknown"), opt_or(b.kernel.as_deref(), "unknown"));
        let sig = if major_minor(ka) != major_minor(kb) { Medium } else { Low };
        out.push(change(sig, "system.kernel", "kernel", "Kernel").values(ka, kb));
    }
    if a.architecture != b.architecture {
        out.push(
            change(Medium, "system.architecture", "architecture", "Architecture")
                .values(&a.architecture, &b.architecture),
        );
    }
    if let (Some(ba), Some(bb)) = (a.boot_time, b.boot_time) {
        if (bb - ba).num_seconds().abs() > BOOT_TOLERANCE_SECS {
            let mut c = change(Medium, "system.reboot", "boot", "System rebooted")
                .field("boot time")
                .values(time(ba), time(bb));
            if let Some(up) = b.uptime_seconds {
                c = c.delta(format!("up {}", hostprint_model::format::duration(up)));
            }
            out.push(c);
        }
    }
    if a.timezone != b.timezone {
        out.push(
            change(Low, "system.timezone", "timezone", "Timezone")
                .values(opt_or(a.timezone.as_deref(), "unknown"), opt_or(b.timezone.as_deref(), "unknown")),
        );
    }
    if a.hardware != b.hardware {
        out.push(
            change(Low, "system.hardware", "hardware", "Hardware")
                .values(opt_or(a.hardware.as_deref(), "unknown"), opt_or(b.hardware.as_deref(), "unknown")),
        );
    }
    if a.container != b.container {
        out.push(
            change(Info, "system.container", "container", "Container runtime")
                .values(opt_or(a.container.as_deref(), "none"), opt_or(b.container.as_deref(), "none")),
        );
    }
}

fn major_minor(kernel: &str) -> (&str, &str) {
    let mut parts = kernel.split(['.', '-']);
    let major = major_version(parts.next().unwrap_or(""));
    (major, parts.next().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, DiffOptions, Significance};

    #[test]
    fn uptime_alone_is_not_a_change() {
        let a = baseline();
        let b = later(&a, 5 * 3600);
        assert!(diff(&a, &b, &DiffOptions::default()).changes.is_empty());
    }

    #[test]
    fn detects_reboot_and_kernel_changes() {
        let a = baseline();
        let mut b = later(&a, 3600);
        let host = b.host.as_mut().unwrap();
        host.boot_time = Some(at(3000));
        host.uptime_seconds = Some(600);
        host.kernel = Some("6.8.0-47-generic".into());
        let d = diff(&a, &b, &DiffOptions::default());
        let rules: Vec<_> = d.changes.iter().map(|c| (c.rule.as_str(), c.significance)).collect();
        assert_eq!(rules, [("system.reboot", Significance::Medium), ("system.kernel", Significance::Low)]);
        assert_eq!(d.changes[0].delta.as_deref(), Some("up 10m"));

        b.host.as_mut().unwrap().kernel = Some("6.11.0-8-generic".into());
        let d = diff(&a, &b, &DiffOptions::default());
        assert_eq!(d.changes.iter().find(|c| c.rule == "system.kernel").unwrap().significance, Significance::Medium);
    }
}
