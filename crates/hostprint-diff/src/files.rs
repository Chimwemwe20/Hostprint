use crate::{time, Category, Change, Significance, Significance::*};
use hostprint_model::format::bytes;
use hostprint_model::FileFingerprint;
use std::collections::BTreeMap;

pub(crate) fn compare(a: &[FileFingerprint], b: &[FileFingerprint], out: &mut Vec<Change>) {
    let index = |v: &[FileFingerprint]| v.iter().map(|f| (f.path.clone(), f.clone())).collect::<BTreeMap<_, _>>();
    let (ia, ib) = (index(a), index(b));
    let change = |sig: Significance, rule: &str, path: &str, field: &str| {
        Change::new(sig, Category::Files, rule, format!("files/{path}/{field}"), path)
    };

    for (path, x) in &ia {
        let Some(y) = ib.get(path) else {
            out.push(
                change(Info, "file.untracked", path, "tracked").field("tracked").values("yes", "no (not configured)"),
            );
            continue;
        };
        if x.exists != y.exists {
            let presence = |f: &FileFingerprint| if f.exists { "present" } else { "missing" };
            let (sig, rule) = if y.exists { (Low, "file.created") } else { (High, "file.removed") };
            out.push(change(sig, rule, path, "exists").field("exists").values(presence(x), presence(y)));
            continue;
        }
        if !y.exists {
            continue;
        }
        match (&x.sha256, &y.sha256) {
            (Some(ha), Some(hb)) if ha != hb => {
                let size = |f: &FileFingerprint| f.size.map(bytes).unwrap_or_else(|| "?".into());
                out.push(
                    change(Medium, "file.content", path, "sha256")
                        .field("sha256")
                        .values(&ha[..12.min(ha.len())], &hb[..12.min(hb.len())])
                        .delta(format!("size {} → {}", size(x), size(y))),
                );
            }
            (Some(_), Some(_)) if x.modified != y.modified => {
                let m = |f: &FileFingerprint| f.modified.map(time).unwrap_or_else(|| "unknown".into());
                out.push(
                    change(Info, "file.touched", path, "mtime")
                        .field("modified (content unchanged)")
                        .values(m(x), m(y)),
                );
            }
            _ => {}
        }
        if x.mode != y.mode && x.mode.is_some() && y.mode.is_some() {
            out.push(
                change(Low, "file.mode", path, "mode")
                    .field("mode")
                    .values(x.mode.clone().unwrap(), y.mode.clone().unwrap()),
            );
        }
        if (x.uid, x.gid) != (y.uid, y.gid) && x.uid.is_some() && y.uid.is_some() {
            let owner = |f: &FileFingerprint| format!("{}:{}", f.uid.unwrap_or(0), f.gid.unwrap_or(0));
            out.push(change(Low, "file.owner", path, "owner").field("owner uid:gid").values(owner(x), owner(y)));
        }
        if x.error != y.error {
            let e = |f: &FileFingerprint| f.error.clone().unwrap_or_else(|| "none".into());
            out.push(change(Low, "file.error", path, "error").field("read error").values(e(x), e(y)));
        }
    }
    for (path, _) in ib.iter().filter(|(p, _)| !ia.contains_key(*p)) {
        out.push(change(Info, "file.tracked", path, "tracked").field("tracked").values("no", "yes"));
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, DiffOptions, Significance::*};

    #[test]
    fn content_change_and_removal() {
        let a = baseline();
        let mut b = later(&a, 600);
        let f = &mut b.files.as_mut().unwrap()[0];
        f.sha256 = Some("4a1c0000000000000000000000000000000000000000000000000000000000aa".into());
        f.size = Some(1700);
        f.modified = Some(at(300));
        let d = diff(&a, &b, &DiffOptions::default());
        assert_eq!(d.changes.len(), 1);
        assert_eq!((d.changes[0].rule.as_str(), d.changes[0].significance), ("file.content", Medium));
        assert_eq!(d.changes[0].delta.as_deref(), Some("size 1.5 KiB → 1.7 KiB"));

        let f = &mut b.files.as_mut().unwrap()[0];
        *f = hostprint_model::FileFingerprint {
            path: f.path.clone(),
            exists: false,
            size: None,
            modified: None,
            sha256: None,
            mode: None,
            uid: None,
            gid: None,
            error: None,
        };
        let d = diff(&a, &b, &DiffOptions::default());
        assert_eq!((d.changes[0].rule.as_str(), d.changes[0].significance), ("file.removed", High));
    }
}
