use crate::{Category, Change, Significance::*};
use chrono::{DateTime, Utc};
use hostprint_model::format::{duration, signed};
use hostprint_model::{LogSource, Logs};
use std::collections::{BTreeMap, BTreeSet};

/// New error messages reported per source.
const MAX_NEW_PATTERNS: usize = 3;
const MAX_EXAMPLE: usize = 100;

pub(crate) fn compare(
    a: &Logs,
    b: &Logs,
    at_a: DateTime<Utc>,
    at_b: DateTime<Utc>,
    out: &mut Vec<Change>,
    notes: &mut Vec<String>,
) {
    let (wa, wb) = ((at_a - a.since).num_seconds().max(0), (at_b - b.since).num_seconds().max(0));
    if wa.abs_diff(wb) * 10 > wa.max(wb) as u64 {
        notes.push(format!(
            "Log windows differ ({} vs {}); error counts are not directly comparable.",
            duration(wa as u64),
            duration(wb as u64)
        ));
    }
    let (ia, ib) = (index(a), index(b));
    let window = format!("last {}", duration(wb as u64));
    let keys: BTreeSet<&(String, String)> = ia.keys().chain(ib.keys()).collect();

    for key in keys {
        let (x, y) = (ia.get(key).copied(), ib.get(key).copied());
        let (ea, eb) = (x.map_or(0, |s| s.errors), y.map_or(0, |s| s.errors));
        let source = y.or(x).expect("key comes from one of the maps");
        let k = |field: &str| format!("logs/{}/{}/{field}", source.kind, source.name);
        let change = |sig, rule, field: &str| {
            Change::new(sig, Category::Logs, rule, k(field), source.name.clone())
                .field(format!("{} {field}", source.kind))
        };

        let sig = if eb == 0 {
            (ea > 0).then_some(Info)
        } else if ea == 0 || (eb >= 3 * ea && eb - ea >= 10) {
            Some(Medium)
        } else if eb > ea {
            Some(Low)
        } else {
            None
        };
        if let Some(sig) = sig {
            out.push(
                change(sig, "log.errors", "errors")
                    .values(ea.to_string(), eb.to_string())
                    .delta(format!("{}, {window}", signed(i64::from(eb) - i64::from(ea)))),
            );
        }

        let Some(y) = y else { continue };
        let known: BTreeSet<&str> =
            x.map(|s| s.top_errors.iter().map(|p| p.pattern.as_str()).collect()).unwrap_or_default();
        for pattern in y.top_errors.iter().filter(|p| !known.contains(p.pattern.as_str())).take(MAX_NEW_PATTERNS) {
            out.push(
                Change::new(Low, Category::Logs, "log.new_error", format!("{}/{}", k("new"), pattern.pattern), &y.name)
                    .field(format!("{} new error", y.kind))
                    .added(clip(&pattern.example, MAX_EXAMPLE))
                    .delta(format!("×{}", pattern.count)),
            );
        }
    }
}

fn index(logs: &Logs) -> BTreeMap<(String, String), &LogSource> {
    logs.sources.iter().map(|s| ((s.kind.clone(), s.name.clone()), s)).collect()
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};
    use chrono::Duration;

    fn rules(changes: &[Change]) -> Vec<(&str, &str, Significance)> {
        changes.iter().map(|c| (c.subject.as_str(), c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn error_bursts_and_new_errors() {
        let a = baseline();
        let mut b = later(&a, 600);
        let logs = b.logs.as_mut().unwrap();
        logs.since = b.captured_at - Duration::seconds(1800);
        logs.sources[0] = log_source("docker", "redis", 140, &[("FATAL: cannot open append-only file #", 17)]);
        let d = diff(&a, &b, &DiffOptions::default());
        assert_eq!(
            rules(&d.changes),
            [("redis", "log.errors", Medium), ("redis", "log.new_error", Low)],
            "{:#?}",
            d.changes
        );
        assert_eq!(d.changes[0].delta.as_deref(), Some("+17, last 30m"));
        assert_eq!(d.changes[1].after.as_deref(), Some("FATAL: cannot open append-only file 42"));
        assert!(d.notes.is_empty(), "{:?}", d.notes);
    }

    #[test]
    fn steady_errors_are_quiet_and_different_windows_are_noted() {
        let mut a = baseline();
        a.logs.as_mut().unwrap().sources[0] = log_source("docker", "redis", 120, &[("timeout #", 4)]);
        let mut b = later(&a, 600);
        b.logs.as_mut().unwrap().sources[0] = log_source("docker", "redis", 120, &[("timeout #", 5)]);
        let d = diff(&a, &b, &DiffOptions::default());
        assert_eq!(rules(&d.changes), [("redis", "log.errors", Low)]);
        assert_eq!(d.notes.len(), 0);

        b.logs.as_mut().unwrap().since = b.captured_at - Duration::hours(4);
        let d = diff(&a, &b, &DiffOptions::default());
        assert!(d.notes[0].starts_with("Log windows differ (30m vs 4h)"), "{:?}", d.notes);
    }
}
