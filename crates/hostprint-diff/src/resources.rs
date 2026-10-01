use crate::{Category, Change, Significance, Significance::*};
use hostprint_model::format::{bytes, percent_change};
use hostprint_model::{Disk, Resources};
use std::collections::BTreeMap;

const MIB: u64 = 1024 * 1024;

pub(crate) fn compare(a: &Resources, b: &Resources, out: &mut Vec<Change>) {
    memory(a, b, out);
    swap(a, b, out);
    cpu(a, b, out);
    load(a, b, out);
    pressure(a, b, out);
    disks(&a.disks, &b.disks, out);
}

fn change(sig: Significance, rule: &str, key: &str, subject: &str) -> Change {
    Change::new(sig, Category::Resources, rule, format!("resources/{key}"), subject)
}

fn memory(a: &Resources, b: &Resources, out: &mut Vec<Change>) {
    let (ma, mb) = (&a.memory, &b.memory);
    if ma.total_bytes > 0 && relative(ma.total_bytes as f64, mb.total_bytes as f64).abs() > 0.02 {
        out.push(
            change(Medium, "memory.total", "memory/total", "Memory total")
                .values(bytes(ma.total_bytes), bytes(mb.total_bytes)),
        );
    }
    if ma.available_bytes == 0 || mb.total_bytes == 0 {
        return;
    }
    let rel = relative(ma.available_bytes as f64, mb.available_bytes as f64);
    let share_after = mb.available_bytes as f64 / mb.total_bytes as f64;
    let sig = if rel < 0.0 {
        let drop = -rel;
        if share_after < 0.10 && drop >= 0.20 {
            Some(High)
        } else if drop >= 0.50 {
            Some(Medium)
        } else if drop >= 0.20 {
            Some(Low)
        } else {
            None
        }
    } else if rel >= 0.50 {
        Some(Info)
    } else {
        None
    };
    if let Some(sig) = sig {
        let pct = percent_change(ma.available_bytes as f64, mb.available_bytes as f64).unwrap_or_default();
        out.push(
            change(sig, "memory.available", "memory/available", "Memory available")
                .values(bytes(ma.available_bytes), bytes(mb.available_bytes))
                .delta(format!("{pct}, {:.0}% of total", share_after * 100.0)),
        );
    }
}

fn swap(a: &Resources, b: &Resources, out: &mut Vec<Change>) {
    let (sa, sb) = (&a.swap, &b.swap);
    if sa.total_bytes != sb.total_bytes {
        out.push(
            change(Low, "swap.total", "swap/total", "Swap total").values(bytes(sa.total_bytes), bytes(sb.total_bytes)),
        );
    }
    if sb.used_bytes > sa.used_bytes {
        let grew = sb.used_bytes - sa.used_bytes;
        if grew >= 256 * MIB && (sa.used_bytes == 0 || sb.used_bytes >= 2 * sa.used_bytes) {
            let heavy = sb.total_bytes > 0 && sb.used_bytes as f64 / sb.total_bytes as f64 >= 0.5;
            out.push(
                change(if heavy { Medium } else { Low }, "swap.used", "swap/used", "Swap used")
                    .values(bytes(sa.used_bytes), bytes(sb.used_bytes))
                    .delta(format!("+{}", bytes(grew))),
            );
        }
    }
}

fn cpu(a: &Resources, b: &Resources, out: &mut Vec<Change>) {
    if a.cpu.logical_cores != b.cpu.logical_cores {
        out.push(
            change(Medium, "cpu.cores", "cpu/cores", "CPU cores")
                .values(a.cpu.logical_cores.to_string(), b.cpu.logical_cores.to_string()),
        );
    }
    // CPU figures come from a sub-second sample, so only large swings count.
    let pct = |v: f64| format!("{v:.0}%");
    if let (Some(ua), Some(ub)) = (a.cpu.usage_percent, b.cpu.usage_percent) {
        let sig = if ub >= 90.0 && ua < 70.0 {
            Some(Medium)
        } else if ub >= 70.0 && ua < 40.0 {
            Some(Low)
        } else {
            None
        };
        if let Some(sig) = sig {
            out.push(change(sig, "cpu.usage", "cpu/usage", "CPU busy").field("sampled").values(pct(ua), pct(ub)));
        }
    }
    if let (Some(ia), Some(ib)) = (a.cpu.iowait_percent, b.cpu.iowait_percent) {
        if ib >= 20.0 && ia < 5.0 {
            out.push(change(Low, "cpu.iowait", "cpu/iowait", "CPU iowait").field("sampled").values(pct(ia), pct(ib)));
        }
    }
    if let (Some(sa), Some(sb)) = (a.cpu.steal_percent, b.cpu.steal_percent) {
        if sb >= 10.0 && sa < 2.0 {
            out.push(change(Low, "cpu.steal", "cpu/steal", "CPU steal").field("sampled").values(pct(sa), pct(sb)));
        }
    }
}

fn load(a: &Resources, b: &Resources, out: &mut Vec<Change>) {
    let (Some(la), Some(lb)) = (a.load, b.load) else { return };
    let ra = la.one / f64::from(a.cpu.logical_cores.max(1));
    let rb = lb.one / f64::from(b.cpu.logical_cores.max(1));
    let sig = if rb >= 2.0 && ra < 2.0 {
        Some(High)
    } else if rb >= 1.0 && ra < 1.0 {
        Some(Medium)
    } else if lb.one >= 2.0 * la.one && rb >= 0.5 {
        Some(Low)
    } else if ra >= 1.0 && rb < 1.0 {
        Some(Info)
    } else {
        None
    };
    if let Some(sig) = sig {
        out.push(
            change(sig, "load.average", "load", "Load average (1m)")
                .values(format!("{:.2}", la.one), format!("{:.2}", lb.one))
                .delta(format!("{:.1}× per core, {} cores", rb, b.cpu.logical_cores)),
        );
    }
}

fn pressure(a: &Resources, b: &Resources, out: &mut Vec<Change>) {
    let (Some(pa), Some(pb)) = (a.pressure, b.pressure) else { return };
    let checks = [
        ("memory/full", "Memory pressure (full)", pa.memory_full_avg60, pb.memory_full_avg60, 10.0, High),
        ("memory/some", "Memory pressure", pa.memory_some_avg60, pb.memory_some_avg60, 10.0, Medium),
        ("io/full", "I/O pressure (full)", pa.io_full_avg60, pb.io_full_avg60, 10.0, Medium),
        ("io/some", "I/O pressure", pa.io_some_avg60, pb.io_some_avg60, 25.0, Medium),
        ("cpu/some", "CPU pressure", pa.cpu_some_avg60, pb.cpu_some_avg60, 50.0, Low),
    ];
    for (key, subject, before, after, threshold, sig) in checks {
        if let (Some(x), Some(y)) = (before, after) {
            if y >= threshold && x < threshold {
                out.push(
                    change(sig, "pressure.stall", &format!("pressure/{key}"), subject)
                        .field("stalled, 60s avg")
                        .values(format!("{x:.1}%"), format!("{y:.1}%")),
                );
            }
        }
    }
}

fn disks(a: &[Disk], b: &[Disk], out: &mut Vec<Change>) {
    let index = |disks: &[Disk]| disks.iter().map(|d| (d.mount_point.clone(), d.clone())).collect::<BTreeMap<_, _>>();
    let (ia, ib) = (index(a), index(b));
    let describe = |d: &Disk| format!("{} ({})", d.device, d.filesystem);
    let key = |mp: &str, field: &str| format!("disks/{mp}/{field}");

    for (mp, da) in &ia {
        let Some(db) = ib.get(mp) else {
            out.push(change(Medium, "disk.unmounted", &key(mp, "mount"), mp).field("mount").removed(describe(da)));
            continue;
        };
        if db.unresponsive != da.unresponsive {
            let (sig, rule) = if db.unresponsive { (High, "disk.unresponsive") } else { (Low, "disk.responsive") };
            let state = |unresponsive: bool| if unresponsive { "not responding" } else { "responding" };
            out.push(
                change(sig, rule, &key(mp, "responsive"), mp)
                    .field("filesystem")
                    .values(state(da.unresponsive), state(db.unresponsive)),
            );
        }
        if db.read_only != da.read_only {
            let sig = if db.read_only { High } else { Low };
            let mode = |ro: bool| if ro { "read-only" } else { "read-write" };
            out.push(
                change(sig, "disk.read_only", &key(mp, "mode"), mp)
                    .field("mount mode")
                    .values(mode(da.read_only), mode(db.read_only)),
            );
        }
        if da.device != db.device || da.filesystem != db.filesystem {
            out.push(
                change(Medium, "disk.device", &key(mp, "device"), mp)
                    .field("device")
                    .values(describe(da), describe(db)),
            );
        }
        if let (Some(ra), Some(rb)) = (da.usage_ratio(), db.usage_ratio()) {
            let sig = if rb >= 0.95 && ra < 0.95 {
                Some(High)
            } else if rb >= 0.90 && ra < 0.90 {
                Some(Medium)
            } else if rb - ra >= 0.05 {
                Some(Low)
            } else if ra - rb >= 0.10 {
                Some(Info)
            } else {
                None
            };
            if let Some(sig) = sig {
                let describe =
                    |r: f64, d: &Disk| format!("{:.0}% ({} free)", r * 100.0, bytes(d.available_bytes.unwrap_or(0)));
                out.push(
                    change(sig, "disk.usage", &key(mp, "usage"), mp)
                        .field("used")
                        .values(describe(ra, da), describe(rb, db))
                        .delta(format!("{:+.0} pts", (rb - ra) * 100.0)),
                );
            }
        }
        if let (Some(ra), Some(rb)) = (da.inode_usage_ratio(), db.inode_usage_ratio()) {
            let sig = if rb >= 0.95 && ra < 0.95 {
                Some(High)
            } else if rb >= 0.90 && ra < 0.90 {
                Some(Medium)
            } else {
                None
            };
            if let Some(sig) = sig {
                let pct = |r: f64| format!("{:.0}%", r * 100.0);
                out.push(
                    change(sig, "disk.inodes", &key(mp, "inodes"), mp).field("inodes used").values(pct(ra), pct(rb)),
                );
            }
        }
        if let (Some(ta), Some(tb)) = (da.total_bytes, db.total_bytes) {
            if ta > 0 && relative(ta as f64, tb as f64).abs() > 0.01 {
                out.push(change(Low, "disk.size", &key(mp, "size"), mp).field("size").values(bytes(ta), bytes(tb)));
            }
        }
    }
    for (mp, db) in &ib {
        if !ia.contains_key(mp) {
            out.push(change(Low, "disk.mounted", &key(mp, "mount"), mp).field("mount").added(describe(db)));
        }
    }
}

fn relative(before: f64, after: f64) -> f64 {
    if before == 0.0 {
        0.0
    } else {
        (after - before) / before
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};
    use hostprint_model::Snapshot;

    fn changes(edit: impl FnOnce(&mut Snapshot)) -> Vec<Change> {
        let a = baseline();
        let mut b = later(&a, 600);
        edit(&mut b);
        diff(&a, &b, &DiffOptions::default()).changes
    }

    fn rules(changes: &[Change]) -> Vec<(&str, Significance)> {
        changes.iter().map(|c| (c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn small_fluctuations_are_ignored() {
        let c = changes(|b| {
            let r = b.resources.as_mut().unwrap();
            r.memory.available_bytes -= 300 * MIB; // -5%
            r.cpu.usage_percent = Some(35.0);
            r.load = Some(hostprint_model::LoadAverage { one: 0.9, five: 0.6, fifteen: 0.4 });
            r.disks[0].used_bytes = Some(41 * GIB);
            r.disks[0].available_bytes = Some(54 * GIB);
        });
        assert_eq!(c, []);
    }

    #[test]
    fn memory_exhaustion_is_high() {
        let c = changes(|b| b.resources.as_mut().unwrap().memory.available_bytes = 700 * MIB);
        assert_eq!(rules(&c), [("memory.available", High)]);
        assert_eq!(c[0].before.as_deref(), Some("5.4 GiB"));
        assert_eq!(c[0].after.as_deref(), Some("700 MiB"));
        assert_eq!(c[0].delta.as_deref(), Some("-87%, 9% of total"));

        let c = changes(|b| b.resources.as_mut().unwrap().memory.available_bytes = 2 * GIB);
        assert_eq!(rules(&c), [("memory.available", Medium)]);
    }

    #[test]
    fn disk_thresholds_and_read_only() {
        let c = changes(|b| {
            let d = &mut b.resources.as_mut().unwrap().disks[0];
            d.used_bytes = Some(92 * GIB);
            d.available_bytes = Some(3 * GIB);
            d.read_only = true;
        });
        assert_eq!(rules(&c), [("disk.read_only", High), ("disk.usage", High)]);
        assert_eq!(c[1].delta.as_deref(), Some("+55 pts"));
    }

    #[test]
    fn hung_filesystem_is_high() {
        let c = changes(|b| {
            let d = &mut b.resources.as_mut().unwrap().disks[0];
            d.unresponsive = true;
            d.used_bytes = None;
            d.available_bytes = None;
            d.total_bytes = None;
            d.inodes_free = None;
            d.inodes_total = None;
        });
        assert_eq!(rules(&c), [("disk.unresponsive", High)]);
    }

    #[test]
    fn load_and_swap() {
        let c = changes(|b| {
            let r = b.resources.as_mut().unwrap();
            r.load = Some(hostprint_model::LoadAverage { one: 9.0, five: 5.0, fifteen: 2.0 });
            r.swap.used_bytes = 1536 * MIB;
            r.swap.free_bytes = 512 * MIB;
        });
        assert_eq!(rules(&c), [("load.average", High), ("swap.used", Medium)]);
    }
}
