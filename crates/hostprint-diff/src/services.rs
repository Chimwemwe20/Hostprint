use crate::{matches_any, Category, Change, DiffOptions, Significance, Significance::*};
use hostprint_model::format::signed;
use hostprint_model::Service;
use std::collections::BTreeMap;

pub(crate) fn compare(a: &[Service], b: &[Service], opts: &DiffOptions, out: &mut Vec<Change>) {
    let index = |v: &[Service]| {
        v.iter()
            .filter(|s| !matches_any(&opts.ignore_services, &s.name))
            .map(|s| (s.name.clone(), s.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (ia, ib) = (index(a), index(b));
    let change = |sig, rule, name: &str, field: &str| {
        Change::new(sig, Category::Services, rule, format!("services/{name}/{field}"), name)
    };

    for (name, x) in &ia {
        let Some(y) = ib.get(name) else {
            // systemd unloads inactive units nobody references, so an inactive
            // unit vanishing from the list is routine.
            let sig = if x.active_state == "active" { Medium } else { Info };
            out.push(change(sig, "service.removed", name, "state").field("state").removed(state(x)));
            continue;
        };
        let oneshot = [x, y].iter().any(|s| s.service_type.as_deref() == Some("oneshot"));
        let routine = if oneshot { Info } else { Low };

        if state(x) != state(y) {
            let (sig, rule): (Significance, &str) = match (x.active_state.as_str(), y.active_state.as_str()) {
                (from, "failed") if from != "failed" => (High, "service.failed"),
                (_, "activating") if y.sub_state == "auto-restart" => (High, "service.restart_loop"),
                ("active", "inactive" | "deactivating") => (if oneshot { Info } else { High }, "service.stopped"),
                ("failed", "active") => (Low, "service.recovered"),
                (from, "active") if from != "active" => (routine, "service.started"),
                _ => (routine, "service.state"),
            };
            out.push(change(sig, rule, name, "state").field("state").values(state(x), state(y)));
        }

        if let (Some(ra), Some(rb)) = (x.restarts, y.restarts) {
            if rb > ra {
                let sig = if rb - ra >= 5 { High } else { Medium };
                out.push(
                    change(sig, "service.restarts", name, "restarts")
                        .field("automatic restarts")
                        .values(ra.to_string(), rb.to_string())
                        .delta(signed(i64::from(rb) - i64::from(ra))),
                );
            }
        }

        let both_running = x.active_state == "active" && y.active_state == "active";
        if both_running && !oneshot && x.restarts == y.restarts && x.active_since != y.active_since {
            out.push(change(Low, "service.restarted", name, "since").field("active since").values(
                x.active_since.as_deref().unwrap_or("unknown"),
                y.active_since.as_deref().unwrap_or("unknown"),
            ));
        }
    }

    for (name, y) in &ib {
        if ia.contains_key(name) {
            continue;
        }
        let (sig, rule) = match y.active_state.as_str() {
            "failed" => (High, "service.failed"),
            "active" => (Low, "service.added"),
            _ => (Info, "service.added"),
        };
        out.push(change(sig, rule, name, "state").field("state").added(state(y)));
    }
}

fn state(s: &Service) -> String {
    format!("{} ({})", s.active_state, s.sub_state)
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};

    fn changes(edit: impl FnOnce(&mut Vec<hostprint_model::Service>)) -> Vec<Change> {
        let a = baseline();
        let mut b = later(&a, 600);
        edit(b.services.as_mut().unwrap());
        diff(&a, &b, &DiffOptions::default()).changes
    }

    fn rules(changes: &[Change]) -> Vec<(&str, &str, Significance)> {
        changes.iter().map(|c| (c.subject.as_str(), c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn stopped_failed_and_restarting_services() {
        let c = changes(|s| {
            s[0].active_state = "failed".into();
            s[0].sub_state = "failed".into();
            s[1].active_state = "activating".into();
            s[1].sub_state = "auto-restart".into();
            s[1].restarts = Some(17);
        });
        assert_eq!(
            rules(&c),
            [
                ("nginx.service", "service.failed", High),
                ("postgresql.service", "service.restarts", High),
                ("postgresql.service", "service.restart_loop", High),
            ]
        );
        assert_eq!(c[0].before.as_deref(), Some("active (running)"));
        assert_eq!(c[0].after.as_deref(), Some("failed (failed)"));
    }

    #[test]
    fn oneshot_churn_is_info() {
        let c = changes(|s| {
            s[2].active_state = "activating".into();
            s[2].sub_state = "start".into();
        });
        assert_eq!(rules(&c), [("apt-daily.service", "service.state", Info)]);
    }

    #[test]
    fn manual_restart_and_unloaded_units() {
        let c = changes(|s| {
            s[0].active_since = Some("Thu 2026-10-01 05:00:00 UTC".into());
            s.retain(|svc| svc.name != "apt-daily.service");
        });
        assert_eq!(
            rules(&c),
            [("nginx.service", "service.restarted", Low), ("apt-daily.service", "service.removed", Info)]
        );
    }
}
