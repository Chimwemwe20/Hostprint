use crate::{major_version, set_delta, Category, Change, Significance::*};
use hostprint_model::{Git, Runtime};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SUBJECT: usize = 48;

pub(crate) fn compare_git(a: &Git, b: &Git, out: &mut Vec<Change>) {
    let repo = b.root.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&b.root).to_string();
    let change =
        |sig, rule, field: &str| Change::new(sig, Category::Application, rule, format!("git/{field}"), repo.clone());

    if a.root != b.root {
        out.push(change(Info, "git.root", "root").field("repository").values(&a.root, &b.root));
    }
    if a.branch != b.branch {
        let branch = |g: &Git| g.branch.clone().unwrap_or_else(|| "(detached)".into());
        out.push(change(Medium, "git.branch", "branch").field("branch").values(branch(a), branch(b)));
    }
    if a.commit != b.commit {
        out.push(change(Medium, "git.commit", "commit").field("commit").values(commit(a), commit(b)));
    }
    if a.dirty != b.dirty {
        let (sig, rule) = if b.dirty { (Medium, "git.dirty") } else { (Low, "git.clean") };
        out.push(change(sig, rule, "tree").field("working tree").values(tree(a), tree(b)));
    } else if a.dirty && a.changed_paths != b.changed_paths {
        let (pa, pb): (BTreeSet<&str>, BTreeSet<&str>) = (
            a.changed_paths.iter().map(String::as_str).collect(),
            b.changed_paths.iter().map(String::as_str).collect(),
        );
        out.push(
            change(Low, "git.changes", "tree")
                .field("changed files")
                .values(tree(a), tree(b))
                .delta(set_delta(pb.difference(&pa).copied(), pa.difference(&pb).copied())),
        );
    }
    if a.untracked != b.untracked && a.dirty == b.dirty {
        out.push(
            change(Info, "git.untracked", "untracked")
                .field("untracked files")
                .values(a.untracked.to_string(), b.untracked.to_string()),
        );
    }
    if a.remote != b.remote {
        let remote = |g: &Git| g.remote.clone().unwrap_or_else(|| "none".into());
        out.push(change(Low, "git.remote", "remote").field("origin").values(remote(a), remote(b)));
    }
}

fn commit(g: &Git) -> String {
    match (g.short_commit(), &g.commit_subject) {
        (Some(sha), Some(subject)) => {
            let subject = match subject.char_indices().nth(MAX_SUBJECT) {
                Some((i, _)) => format!("{}…", &subject[..i]),
                None => subject.clone(),
            };
            format!("{sha} {subject}")
        }
        (Some(sha), None) => sha.to_string(),
        (None, _) => "(no commits)".to_string(),
    }
}

fn tree(g: &Git) -> String {
    if !g.dirty {
        return "clean".into();
    }
    let mut parts = Vec::new();
    if g.modified > 0 {
        parts.push(format!("{} modified", g.modified));
    }
    if g.staged > 0 {
        parts.push(format!("{} staged", g.staged));
    }
    parts.join(", ")
}

pub(crate) fn compare_runtimes(a: &[Runtime], b: &[Runtime], out: &mut Vec<Change>) {
    let index = |v: &[Runtime]| v.iter().map(|r| (r.name.clone(), r.clone())).collect::<BTreeMap<_, _>>();
    let (ia, ib) = (index(a), index(b));
    let version = |r: &Runtime| r.version.clone().unwrap_or_else(|| "unknown".into());
    let change = |sig, rule, name: &str, field: &str| {
        Change::new(sig, Category::Application, rule, format!("runtimes/{name}/{field}"), name)
    };
    for (name, x) in &ia {
        match ib.get(name) {
            None => out.push(change(Medium, "runtime.removed", name, "present").field("installed").removed(version(x))),
            Some(y) => {
                if x.version != y.version {
                    let major_changed = match (&x.version, &y.version) {
                        (Some(va), Some(vb)) => major_version(va) != major_version(vb),
                        _ => false,
                    };
                    out.push(
                        change(if major_changed { Medium } else { Low }, "runtime.version", name, "version")
                            .field("version")
                            .values(version(x), version(y)),
                    );
                }
                if x.path != y.path {
                    out.push(change(Low, "runtime.path", name, "path").field("path").values(&x.path, &y.path));
                }
            }
        }
    }
    for (name, y) in &ib {
        if !ia.contains_key(name) {
            out.push(change(Low, "runtime.added", name, "present").field("installed").added(version(y)));
        }
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

    fn rules(changes: &[Change]) -> Vec<(&str, &str, Significance)> {
        changes.iter().map(|c| (c.subject.as_str(), c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn commit_and_dirty_tree() {
        let c = changes(|b| {
            let g = b.git.as_mut().unwrap();
            g.commit = Some("e821aa4000000000000000000000000000000000".into());
            g.commit_subject = Some("Raise DATABASE_POOL_SIZE".into());
            g.dirty = true;
            g.modified = 2;
            g.changed_paths = vec!["config/prod.toml".into(), "src/db.rs".into()];
        });
        assert_eq!(rules(&c), [("app", "git.commit", Medium), ("app", "git.dirty", Medium)]);
        assert_eq!(c[0].before.as_deref(), Some("b921cc1 Tune connection pool"));
        assert_eq!(c[0].after.as_deref(), Some("e821aa4 Raise DATABASE_POOL_SIZE"));
        assert_eq!(c[1].after.as_deref(), Some("2 modified"));
    }

    #[test]
    fn runtime_versions() {
        let c = changes(|b| {
            let r = b.runtimes.as_mut().unwrap();
            r[0].version = Some("23.0.0".into());
            r[1].version = Some("3.12.4".into());
            r.push(hostprint_model::Runtime {
                name: "go".into(),
                version: Some("1.23.1".into()),
                path: "/usr/bin/go".into(),
            });
        });
        assert_eq!(
            rules(&c),
            [("node", "runtime.version", Medium), ("go", "runtime.added", Low), ("python3", "runtime.version", Low),]
        );
    }
}
