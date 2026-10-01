use crate::{matches_any, set_delta, Category, Change, DiffOptions, Significance::*};
use hostprint_model::{EnvVar, Environment};
use std::collections::BTreeMap;

/// Variables that describe the terminal session rather than the system.
const VOLATILE: &[&str] = &[
    "_",
    "OLDPWD",
    "PWD",
    "SHLVL",
    "COLUMNS",
    "LINES",
    "TERM_SESSION_ID",
    "SSH_CLIENT",
    "SSH_CONNECTION",
    "SSH_TTY",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_AGENT_INFO",
    "WINDOWID",
    "DISPLAY",
    "XAUTHORITY",
    "XDG_SESSION_ID",
    "XDG_SESSION_CLASS",
    "XDG_SESSION_TYPE",
    "XDG_VTNR",
    "XDG_SEAT",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "SYSTEMD_EXEC_PID",
    "MANAGERPID",
    "DBUS_SESSION_BUS_ADDRESS",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "WINDOW",
    "TERM_PROGRAM_VERSION",
    // Which terminal and shell Hostprint was started from (found when a
    // capture made inside tmux was compared with one made outside it).
    "TERM",
    "TERM_PROGRAM",
    "COLORTERM",
    "SHELL",
    // macOS session bookkeeping.
    "XPC_SERVICE_NAME",
    "XPC_FLAGS",
    "__CFBundleIdentifier",
    "SECURITYSESSIONID",
    "LaunchInstanceID",
    "MAIL",
    "SUDO_COMMAND",
    "SUDO_UID",
    "SUDO_GID",
    "SUDO_USER",
    "LS_COLORS",
    "HISTFILE",
    "OLDCWD",
];
const VOLATILE_PREFIXES: &[&str] = &[
    "VSCODE_",
    "KITTY_",
    "WT_",
    "ALACRITTY_",
    "GNOME_TERMINAL_",
    "KONSOLE_",
    "ITERM_",
    "LC_TERMINAL",
    "WEZTERM_",
    "GHOSTTY_",
    // Hostprint's own settings, not the system's.
    "HOSTPRINT_",
    "TERMINAL_",
];

pub(crate) fn compare(
    a: &Environment,
    b: &Environment,
    opts: &DiffOptions,
    out: &mut Vec<Change>,
    notes: &mut Vec<String>,
) {
    let comparable = a.fingerprint_key_id == b.fingerprint_key_id;
    let index = |e: &Environment| {
        e.variables
            .iter()
            .filter(|v| !matches_any(&opts.ignore_env, &v.name))
            .map(|v| ((v.source.clone(), v.name.clone()), v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (ia, ib) = (index(a), index(b));
    let mut uncomparable = 0;

    for (key, x) in &ia {
        let volatile = is_volatile(&x.name);
        let Some(y) = ib.get(key) else {
            out.push(change(if volatile { Info } else { Medium }, "env.removed", x).field("value").removed(display(x)));
            continue;
        };
        if x.redacted != y.redacted {
            out.push(change(Info, "env.redaction", x).field("redaction").values(redaction(x), redaction(y)));
            continue;
        }
        let changed = match (&x.fingerprint, &y.fingerprint) {
            (Some(fa), Some(fb)) if comparable => fa != fb,
            (Some(_), Some(_)) => {
                uncomparable += 1;
                false
            }
            _ => x.value != y.value,
        };
        if !changed {
            continue;
        }
        let c = if volatile {
            change(Info, "env.changed", x).field("value").values(display(x), display(y))
        } else if x.name == "PATH" {
            path_change(x, y)
        } else if x.redacted {
            change(Medium, "env.secret_changed", x)
                .field("secret")
                .values(fingerprint(x), fingerprint(y))
                .delta("value redacted")
        } else {
            change(Medium, "env.changed", x).field("value").values(display(x), display(y))
        };
        out.push(c);
    }
    for (key, y) in &ib {
        if !ia.contains_key(key) {
            let sig = if is_volatile(&y.name) { Info } else { Low };
            out.push(change(sig, "env.added", y).field("value").added(display(y)));
        }
    }
    if uncomparable > 0 {
        notes.push(format!(
            "{uncomparable} redacted values were fingerprinted with different keys (different machines, or a reset key) and could not be compared."
        ));
    }
}

fn change(sig: crate::Significance, rule: &str, v: &EnvVar) -> Change {
    let subject = if v.source == "process" {
        v.name.clone()
    } else {
        let file = v.source.rsplit('/').next().unwrap_or(&v.source);
        format!("{} ({file})", v.name)
    };
    Change::new(sig, Category::Configuration, rule, format!("env/{}/{}", v.source, v.name), subject)
}

fn path_change(x: &EnvVar, y: &EnvVar) -> Change {
    let (pa, pb): (Vec<&str>, Vec<&str>) = (x.value.split(':').collect(), y.value.split(':').collect());
    let added: Vec<&str> = pb.iter().filter(|p| !pa.contains(p)).copied().collect();
    let removed: Vec<&str> = pa.iter().filter(|p| !pb.contains(p)).copied().collect();
    let delta =
        if added.is_empty() && removed.is_empty() { "reordered".to_string() } else { set_delta(added, removed) };
    change(Low, "env.path", x).field("value").values(&x.value, &y.value).delta(delta)
}

fn is_volatile(name: &str) -> bool {
    VOLATILE.contains(&name) || VOLATILE_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn display(v: &EnvVar) -> String {
    v.value.clone()
}

fn fingerprint(v: &EnvVar) -> String {
    match &v.fingerprint {
        Some(fp) => format!("fp {}", &fp[..fp.len().min(8)]),
        None => v.value.clone(),
    }
}

fn redaction(v: &EnvVar) -> &'static str {
    if v.redacted {
        "redacted"
    } else {
        "stored"
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use crate::{diff, Change, DiffOptions, Significance, Significance::*};
    use hostprint_model::Snapshot;

    fn changes(edit: impl FnOnce(&mut Snapshot)) -> (Vec<Change>, Vec<String>) {
        let a = baseline();
        let mut b = later(&a, 600);
        edit(&mut b);
        let d = diff(&a, &b, &DiffOptions::default());
        (d.changes, d.notes)
    }

    fn vars(s: &mut Snapshot) -> &mut Vec<hostprint_model::EnvVar> {
        &mut s.environment.as_mut().unwrap().variables
    }

    fn rules(changes: &[Change]) -> Vec<(&str, &str, Significance)> {
        changes.iter().map(|c| (c.subject.as_str(), c.rule.as_str(), c.significance)).collect()
    }

    #[test]
    fn values_secrets_and_path() {
        let (c, _) = changes(|b| {
            let v = vars(b);
            v[0].value = "50".into();
            v[1].fingerprint = Some("2222222222222222".into());
            v[2].value = "/usr/local/go/bin:/usr/local/bin:/usr/bin:/bin".into();
            v[3].value = "/tmp".into(); // PWD
            v.push(env("FEATURE_FLAG", "on"));
        });
        assert_eq!(
            rules(&c),
            [
                ("DATABASE_POOL_SIZE", "env.changed", Medium),
                ("JWT_SECRET", "env.secret_changed", Medium),
                ("FEATURE_FLAG", "env.added", Low),
                ("PATH", "env.path", Low),
                ("PWD", "env.changed", Info),
            ]
        );
        assert_eq!((c[0].before.as_deref(), c[0].after.as_deref()), (Some("10"), Some("50")));
        assert_eq!(c[1].after.as_deref(), Some("fp 22222222"));
        assert!(!c[1].after.as_deref().unwrap().contains("REDACTED"));
        assert_eq!(c[3].delta.as_deref(), Some("+/usr/local/go/bin"));
    }

    #[test]
    fn terminal_variables_are_info() {
        let (c, _) = changes(|b| {
            let v = vars(b);
            v.push(env("TERM", "tmux-256color"));
            v.push(env("TERM_PROGRAM", "tmux"));
            v.push(env("SHELL", "/bin/bash"));
        });
        assert!(c.iter().all(|c| c.significance == Info), "{c:#?}");
    }

    #[test]
    fn fingerprints_from_other_keys_are_not_compared() {
        let (c, notes) = changes(|b| {
            b.environment.as_mut().unwrap().fingerprint_key_id = "key2".into();
            vars(b)[1].fingerprint = Some("9999999999999999".into());
        });
        assert_eq!(c, []);
        assert_eq!(notes.len(), 1);
    }
}
