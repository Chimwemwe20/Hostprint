use crate::{matches_any, Category, Change, DiffOptions, Significance, Significance::*};
use hostprint_model::format::{bytes, percent_change, signed};
use hostprint_model::{Container, Docker};
use std::collections::BTreeMap;

const MIB: u64 = 1024 * 1024;

pub(crate) fn compare(a: &Docker, b: &Docker, opts: &DiffOptions, out: &mut Vec<Change>) {
    if a.engine_version != b.engine_version {
        let v = |d: &Docker| d.engine_version.clone().unwrap_or_else(|| "unknown".into());
        out.push(
            Change::new(Low, Category::Containers, "docker.engine", "containers/@engine/version", "Docker Engine")
                .field("version")
                .values(v(a), v(b)),
        );
    }
    let index = |d: &Docker| {
        d.containers
            .iter()
            .filter(|c| !matches_any(&opts.ignore_containers, &c.name))
            .map(|c| (c.name.clone(), c.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (ia, ib) = (index(a), index(b));
    let change = |sig: Significance, rule: &str, name: &str, field: &str| {
        Change::new(sig, Category::Containers, rule, format!("containers/{name}/{field}"), name)
    };

    for (name, x) in &ia {
        let Some(y) = ib.get(name) else {
            let sig = if x.state == "running" { High } else { Low };
            out.push(change(sig, "container.removed", name, "present").field("container").removed(describe(x)));
            continue;
        };
        compare_container(x, y, &change, out);
    }
    for (name, y) in &ib {
        if !ia.contains_key(name) {
            let troubled = y.state == "restarting" || y.health.as_deref() == Some("unhealthy");
            out.push(
                change(if troubled { Medium } else { Low }, "container.added", name, "present")
                    .field("container")
                    .added(describe(y)),
            );
        }
    }
}

fn compare_container(
    x: &Container,
    y: &Container,
    change: &impl Fn(Significance, &str, &str, &str) -> Change,
    out: &mut Vec<Change>,
) {
    let name = y.name.as_str();
    if x.state != y.state {
        let sig = match (x.state.as_str(), y.state.as_str()) {
            ("running", _) => High,
            (_, "running") => Low,
            _ => Medium,
        };
        out.push(change(sig, "container.state", name, "state").field("state").values(&x.state, &y.state));
    }
    if x.health != y.health {
        let sig = match (x.health.as_deref(), y.health.as_deref()) {
            (_, Some("unhealthy")) => High,
            (Some("healthy"), Some("starting")) => Medium,
            _ => Low,
        };
        let h = |c: &Container| c.health.clone().unwrap_or_else(|| "no health check".into());
        out.push(change(sig, "container.health", name, "health").field("health").values(h(x), h(y)));
    }
    if y.restart_count > x.restart_count {
        let grew = y.restart_count - x.restart_count;
        let sig = if grew >= 3 || y.state == "restarting" { High } else { Medium };
        out.push(
            change(sig, "container.restarts", name, "restarts")
                .field("restart count")
                .values(x.restart_count.to_string(), y.restart_count.to_string())
                .delta(signed(i64::from(grew))),
        );
    }
    if y.oom_killed && !x.oom_killed {
        out.push(change(High, "container.oom_killed", name, "oom").field("OOM killed").values("no", "yes"));
    }
    if let Some(code) = y.exit_code.filter(|c| *c != 0) {
        if x.exit_code != y.exit_code {
            let mut c = change(Medium, "container.exit_code", name, "exit_code")
                .field("exit code")
                .values(x.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "none".into()), code.to_string());
            if let Some(meaning) = exit_meaning(code) {
                c = c.delta(meaning);
            }
            out.push(c);
        }
    }
    if x.image != y.image {
        out.push(change(Medium, "container.image", name, "image").field("image").values(&x.image, &y.image));
    } else if let (Some(ia), Some(ib)) = (&x.image_id, &y.image_id) {
        if ia != ib {
            out.push(
                change(Medium, "container.image_id", name, "image_id").field("image ID (same tag)").values(ia, ib),
            );
        }
    }
    // Docker only reports port bindings for running containers, so a stopped
    // or restarting container would otherwise appear to have lost its ports.
    if x.ports != y.ports && x.state == "running" && y.state == "running" {
        let p = |c: &Container| if c.ports.is_empty() { "none".to_string() } else { c.ports.join(", ") };
        out.push(change(Medium, "container.ports", name, "ports").field("ports").values(p(x), p(y)));
    }
    memory(x, y, change, out);
    if x.id != y.id {
        out.push(change(Low, "container.recreated", name, "id").field("container ID").values(&x.id, &y.id));
    } else if x.started_at != y.started_at && x.restart_count == y.restart_count && y.state == "running" {
        let s = |c: &Container| c.started_at.clone().unwrap_or_else(|| "unknown".into());
        out.push(change(Low, "container.restarted", name, "started").field("started at").values(s(x), s(y)));
    }
}

fn memory(
    x: &Container,
    y: &Container,
    change: &impl Fn(Significance, &str, &str, &str) -> Change,
    out: &mut Vec<Change>,
) {
    let (Some(ma), Some(mb)) = (x.memory_bytes, y.memory_bytes) else { return };
    let share = |m: u64, limit: Option<u64>| limit.filter(|l| *l > 0).map(|l| m as f64 / l as f64);
    if let (Some(sa), Some(sb)) = (share(ma, x.memory_limit_bytes), share(mb, y.memory_limit_bytes)) {
        if sb >= 0.9 && sa < 0.9 {
            out.push(change(High, "container.memory_limit", &y.name, "memory").field("memory").values(
                format!("{} ({:.0}% of limit)", bytes(ma), sa * 100.0),
                format!("{} ({:.0}% of limit)", bytes(mb), sb * 100.0),
            ));
            return;
        }
    }
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
                change(sig, "container.memory", &y.name, "memory")
                    .field("memory")
                    .values(bytes(ma), bytes(mb))
                    .delta(percent_change(ma as f64, mb as f64).unwrap_or_default()),
            );
        }
    }
}

fn describe(c: &Container) -> String {
    let mut s = c.state.clone();
    if let Some(h) = &c.health {
        s.push_str(", ");
        s.push_str(h);
    }
    s.push_str(" · ");
    s.push_str(&c.image);
    s
}

/// Conventional meaning of common container exit codes.
fn exit_meaning(code: i64) -> Option<&'static str> {
    match code {
        125 => Some("docker run error"),
        126 => Some("command not executable"),
        127 => Some("command not found"),
        134 => Some("SIGABRT"),
        137 => Some("SIGKILL (often OOM)"),
        139 => Some("SIGSEGV"),
        143 => Some("SIGTERM"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};

    fn changes(edit: impl FnOnce(&mut Vec<hostprint_model::Container>)) -> Vec<Change> {
        let a = baseline();
        let mut b = later(&a, 600);
        edit(&mut b.docker.as_mut().unwrap().containers);
        diff(&a, &b, &DiffOptions::default()).changes
    }

    fn rules(changes: &[Change]) -> Vec<(&str, &str, Significance)> {
        changes.iter().map(|c| (c.subject.as_str(), c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn redis_restart_loop() {
        let c = changes(|cs| {
            let redis = &mut cs[1];
            redis.state = "restarting".into();
            redis.health = Some("unhealthy".into());
            redis.restart_count = 17;
            redis.memory_bytes = None;
        });
        assert_eq!(
            rules(&c),
            [
                ("redis", "container.health", High),
                ("redis", "container.restarts", High),
                ("redis", "container.state", High),
            ]
        );
        let restarts = &c[1];
        assert_eq!((restarts.before.as_deref(), restarts.after.as_deref()), (Some("0"), Some("17")));
        assert_eq!(restarts.delta.as_deref(), Some("+17"));
    }

    #[test]
    fn image_rebuild_oom_and_removal() {
        let c = changes(|cs| {
            cs[0].image_id = Some("bbbbbbbbbbbb".into());
            cs[0].oom_killed = true;
            cs[0].state = "exited".into();
            cs[0].exit_code = Some(137);
            cs.retain(|c| c.name != "redis");
        });
        assert_eq!(
            rules(&c),
            [
                ("api", "container.oom_killed", High),
                ("api", "container.state", High),
                ("redis", "container.removed", High),
                ("api", "container.exit_code", Medium),
                ("api", "container.image_id", Medium),
            ]
        );
        assert_eq!(c[3].delta.as_deref(), Some("SIGKILL (often OOM)"));
    }

    #[test]
    fn memory_near_limit() {
        let c = changes(|cs| {
            cs[0].memory_limit_bytes = Some(512 * MIB);
            cs[0].memory_bytes = Some(500 * MIB);
        });
        assert_eq!(rules(&c), [("api", "container.memory_limit", High)]);
    }

    #[test]
    fn ports_are_only_compared_between_running_containers() {
        let mut a = baseline();
        a.docker.as_mut().unwrap().containers[1].ports = vec!["6379/tcp".into()];
        let mut b = later(&a, 60);
        let redis = &mut b.docker.as_mut().unwrap().containers[1];
        redis.state = "restarting".into();
        redis.ports = vec![];
        let c = diff(&a, &b, &DiffOptions::default()).changes;
        assert_eq!(rules(&c), [("redis", "container.state", High)]);

        let redis = &mut b.docker.as_mut().unwrap().containers[1];
        redis.state = "running".into();
        redis.ports = vec!["0.0.0.0:6379->6379/tcp".into()];
        let c = diff(&a, &b, &DiffOptions::default()).changes;
        assert_eq!(rules(&c), [("redis", "container.ports", Medium)]);
    }
}
