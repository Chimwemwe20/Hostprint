use chrono::{DateTime, Utc};
use hostprint_collectors::{CaptureContext, CollectError, Collected, Collector, Section};
use hostprint_model::{CaptureInfo, CollectorReport, CollectorStatus, Snapshot, SCHEMA_VERSION};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

type Outcome = (Result<Collected, CollectError>, Duration);

/// Runs every collector in parallel and assembles the snapshot.
///
/// A collector that fails, panics or overruns `ctx.collector_timeout` is
/// recorded in the capture report and its section left empty; it never
/// aborts or stalls the capture. An overrunning collector's thread is
/// abandoned rather than joined: it is usually blocked in the kernel (a hung
/// mount, a process stuck in D state), and waiting for it would hang an
/// incident investigation on the very thing being investigated.
pub fn capture(name: &str, ctx: &CaptureContext, collectors: &[Arc<dyn Collector>]) -> Snapshot {
    let started = Instant::now();
    let captured_at = Utc::now();
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    let mut results: Vec<Option<Outcome>> = vec![None; collectors.len()];
    for (index, collector) in collectors.iter().enumerate() {
        let (collector, ctx, tx) = (Arc::clone(collector), ctx.clone(), tx.clone());
        let spawned = std::thread::Builder::new().name(format!("collector-{}", collector.name())).spawn(move || {
            let t = Instant::now();
            let result = catch_unwind(AssertUnwindSafe(|| collector.collect(&ctx)))
                .unwrap_or_else(|_| Err(CollectError::Failed("collector panicked".into())));
            let _ = tx.send((index, (result, t.elapsed())));
        });
        if let Err(e) = spawned {
            results[index] = Some((Err(CollectError::Failed(format!("could not start: {e}"))), Duration::ZERO));
        }
    }
    drop(tx);

    let deadline = started + ctx.collector_timeout;
    while results.iter().any(Option::is_none) {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((index, outcome)) => results[index] = Some(outcome),
            Err(_) => break,
        }
    }
    let outcomes: Vec<(CollectorReport, Option<Section>)> = collectors
        .iter()
        .zip(results)
        .map(|(collector, result)| {
            let (result, elapsed) = result.unwrap_or_else(|| {
                let msg = format!("did not finish within {}s", ctx.collector_timeout.as_secs());
                (Err(CollectError::Failed(msg)), ctx.collector_timeout)
            });
            outcome(collector.name(), result, elapsed)
        })
        .collect();

    let (uid, user) = hostprint_collectors::current_user();
    let mut snapshot = Snapshot {
        schema_version: SCHEMA_VERSION,
        id: new_snapshot_id(captured_at),
        name: name.to_string(),
        captured_at,
        capture: CaptureInfo {
            hostprint_version: env!("CARGO_PKG_VERSION").to_string(),
            duration_ms: 0,
            user,
            uid,
            elevated: uid == Some(0),
            working_dir: std::env::current_dir().ok().map(|d| d.display().to_string()),
            remote: None,
            collectors: Vec::with_capacity(outcomes.len()),
        },
        host: None,
        resources: None,
        processes: None,
        network: None,
        services: None,
        docker: None,
        git: None,
        runtimes: None,
        environment: None,
        files: None,
        logs: None,
    };
    for (report, section) in outcomes {
        snapshot.capture.collectors.push(report);
        match section {
            Some(Section::Host(v)) => snapshot.host = Some(v),
            Some(Section::Resources(v)) => snapshot.resources = Some(v),
            Some(Section::Processes(v)) => snapshot.processes = Some(v),
            Some(Section::Network(v)) => snapshot.network = Some(v),
            Some(Section::Services(v)) => snapshot.services = Some(v),
            Some(Section::Docker(v)) => snapshot.docker = Some(v),
            Some(Section::Git(v)) => snapshot.git = Some(v),
            Some(Section::Runtimes(v)) => snapshot.runtimes = Some(v),
            Some(Section::Environment(v)) => snapshot.environment = Some(v),
            Some(Section::Files(v)) => snapshot.files = Some(v),
            Some(Section::Logs(v)) => snapshot.logs = Some(v),
            None => {}
        }
    }
    normalize(&mut snapshot);
    snapshot.capture.duration_ms = started.elapsed().as_millis() as u64;
    snapshot
}

fn outcome(
    name: &str,
    result: Result<Collected, CollectError>,
    elapsed: Duration,
) -> (CollectorReport, Option<Section>) {
    let mut report = CollectorReport {
        name: name.to_string(),
        status: CollectorStatus::Ok,
        duration_ms: elapsed.as_millis() as u64,
        summary: None,
        message: None,
        notes: Vec::new(),
    };
    match result {
        Ok(collected) => {
            if !collected.notes.is_empty() {
                report.status = CollectorStatus::Partial;
            }
            report.summary = collected.summary;
            report.notes = collected.notes;
            (report, Some(collected.section))
        }
        Err(CollectError::Unavailable(msg)) => {
            report.status = CollectorStatus::Skipped;
            report.message = Some(msg);
            (report, None)
        }
        Err(CollectError::Failed(msg)) => {
            report.status = CollectorStatus::Failed;
            report.message = Some(msg);
            (report, None)
        }
    }
}

/// Puts every list into a canonical order so that capturing an unchanged
/// machine twice yields structurally identical snapshots.
pub fn normalize(s: &mut Snapshot) {
    if let Some(r) = &mut s.resources {
        r.disks.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    }
    if let Some(p) = &mut s.processes {
        p.list.sort_by_key(|p| p.pid);
    }
    if let Some(n) = &mut s.network {
        n.interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        for iface in &mut n.interfaces {
            iface.addresses.sort();
        }
        n.listening.sort();
        n.listening.dedup();
        n.default_gateways.sort();
        // DNS server order is significant (it is the resolution order), so it
        // is deliberately left alone.
    }
    if let Some(v) = &mut s.services {
        v.sort_by(|a, b| a.name.cmp(&b.name));
    }
    if let Some(d) = &mut s.docker {
        d.containers.sort_by(|a, b| a.name.cmp(&b.name));
        for c in &mut d.containers {
            c.ports.sort();
        }
    }
    if let Some(g) = &mut s.git {
        g.changed_paths.sort();
    }
    if let Some(v) = &mut s.runtimes {
        v.sort_by(|a, b| a.name.cmp(&b.name));
    }
    if let Some(e) = &mut s.environment {
        e.variables.sort_by(|a, b| (&a.source, &a.name).cmp(&(&b.source, &b.name)));
    }
    if let Some(f) = &mut s.files {
        f.sort_by(|a, b| a.path.cmp(&b.path));
    }
    if let Some(l) = &mut s.logs {
        l.sources.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    }
}

/// A ULID-style identifier: `snap_` + 26 Crockford base32 characters encoding
/// a millisecond timestamp and 80 random bits. Sorts by capture time.
pub fn new_snapshot_id(at: DateTime<Utc>) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = at.timestamp_millis().max(0) as u128 & ((1 << 48) - 1);
    let mut random = [0u8; 10];
    if getrandom::fill(&mut random).is_err() {
        // Fall back to clock entropy; uniqueness within one host is enough.
        let nanos = at.timestamp_subsec_nanos().to_le_bytes();
        random[..4].copy_from_slice(&nanos);
        random[4..8].copy_from_slice(&std::process::id().to_le_bytes());
    }
    let mut value = millis << 80;
    for (i, b) in random.iter().enumerate() {
        value |= (*b as u128) << (8 * (9 - i));
    }
    let mut out = [0u8; 26];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 5 * (25 - i);
        *slot = ALPHABET[((value >> shift) & 0x1f) as usize];
    }
    format!("snap_{}", String::from_utf8_lossy(&out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hostprint_collectors::redact::Redactor;

    struct Fails;
    impl Collector for Fails {
        fn name(&self) -> &'static str {
            "docker"
        }
        fn title(&self) -> &'static str {
            "Docker"
        }
        fn collect(&self, _: &CaptureContext) -> Result<Collected, CollectError> {
            Err(CollectError::Failed("Docker daemon not reachable".into()))
        }
    }

    struct Panics;
    impl Collector for Panics {
        fn name(&self) -> &'static str {
            "git"
        }
        fn title(&self) -> &'static str {
            "Git"
        }
        fn collect(&self, _: &CaptureContext) -> Result<Collected, CollectError> {
            panic!("boom")
        }
    }

    struct Runtimes;
    impl Collector for Runtimes {
        fn name(&self) -> &'static str {
            "runtimes"
        }
        fn title(&self) -> &'static str {
            "Runtimes"
        }
        fn collect(&self, _: &CaptureContext) -> Result<Collected, CollectError> {
            let rt =
                |n: &str| hostprint_model::Runtime { name: n.into(), version: Some("1".into()), path: "/x".into() };
            Ok(Collected::new(Section::Runtimes(vec![rt("zig"), rt("node")])).note("partial"))
        }
    }

    struct Hangs;
    impl Collector for Hangs {
        fn name(&self) -> &'static str {
            "resources"
        }
        fn title(&self) -> &'static str {
            "Resources"
        }
        fn collect(&self, _: &CaptureContext) -> Result<Collected, CollectError> {
            std::thread::sleep(Duration::from_secs(60));
            Err(CollectError::Failed("unreachable".into()))
        }
    }

    #[test]
    fn a_hung_collector_does_not_stall_the_capture() {
        let mut ctx = CaptureContext::new(Redactor::new(b"k"));
        ctx.collector_timeout = Duration::from_millis(200);
        let collectors: Vec<Arc<dyn Collector>> = vec![Arc::new(Hangs), Arc::new(Runtimes)];
        let t = Instant::now();
        let snap = capture("test", &ctx, &collectors);
        assert!(t.elapsed() < Duration::from_secs(5));
        let report = snap.collector("resources").unwrap();
        assert_eq!(report.status, CollectorStatus::Failed);
        assert!(report.message.as_deref().unwrap().starts_with("did not finish within"));
        assert!(snap.runtimes.is_some(), "other collectors still report");
    }

    #[test]
    fn failures_are_recorded_not_fatal() {
        let ctx = CaptureContext::new(Redactor::new(b"k"));
        let collectors: Vec<Arc<dyn Collector>> = vec![Arc::new(Fails), Arc::new(Panics), Arc::new(Runtimes)];
        let snap = capture("test", &ctx, &collectors);
        assert_eq!(snap.collector("docker").unwrap().status, CollectorStatus::Failed);
        assert_eq!(snap.collector("git").unwrap().message.as_deref(), Some("collector panicked"));
        assert_eq!(snap.collector("runtimes").unwrap().status, CollectorStatus::Partial);
        let names: Vec<_> = snap.runtimes.unwrap().into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["node", "zig"], "normalized order");
        assert!(snap.docker.is_none());
    }

    #[test]
    fn snapshot_ids_sort_by_time() {
        let a = new_snapshot_id(DateTime::from_timestamp(1_790_000_000, 0).unwrap());
        let b = new_snapshot_id(DateTime::from_timestamp(1_790_000_001, 0).unwrap());
        assert_eq!(a.len(), 31);
        assert!(a.starts_with("snap_"));
        assert!(a < b);
    }
}
