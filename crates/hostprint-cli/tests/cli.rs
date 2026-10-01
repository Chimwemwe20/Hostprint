//! End-to-end tests against the real binary and the real machine (Linux).

#![cfg(target_os = "linux")]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hostprint-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn hostprint(home: &Path, args: &[&str]) -> Output {
    hostprint_with_env(home, args, &[])
}

fn hostprint_with_env(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hostprint"));
    cmd.args(args).env("HOSTPRINT_HOME", home).env("NO_COLOR", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run hostprint")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn assert_ok(o: &Output) {
    assert!(
        o.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status.code(),
        stdout(o),
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn capture_list_show_diff_delete() {
    let home = temp_home("flow");
    let first = hostprint(&home, &["capture", "--name", "first"]);
    assert_ok(&first);
    assert!(stdout(&first).contains("Snapshot saved: first"), "{}", stdout(&first));
    assert_ok(&hostprint(&home, &["capture", "--name", "second", "--quiet"]));

    let list = hostprint(&home, &["list"]);
    assert_ok(&list);
    assert!(stdout(&list).contains("first") && stdout(&list).contains("second"));

    let show = hostprint(&home, &["show", "first"]);
    assert_ok(&show);
    assert!(stdout(&show).contains("SNAPSHOT first"));
    let json: Value = serde_json::from_slice(&hostprint(&home, &["show", "first", "--json"]).stdout).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["name"], "first");
    assert_ok(&hostprint(&home, &["show", "first", "--section", "processes"]));

    // Two captures of an unchanged machine must not disagree about anything
    // significant: this is the determinism promise.
    let diff = hostprint(&home, &["diff", "first", "second", "--json"]);
    assert_ok(&diff);
    let diff: Value = serde_json::from_slice(&diff.stdout).unwrap();
    let significant: Vec<&Value> = diff["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["significance"] == "high" || c["significance"] == "medium")
        .collect();
    assert!(significant.is_empty(), "unexpected changes between back-to-back captures: {significant:#?}");

    let human = hostprint(&home, &["diff", "first", "second"]);
    assert_ok(&human);
    assert!(stdout(&human).starts_with("HOSTPRINT DIFF"));

    // Live comparison without a second stored snapshot.
    assert_ok(&hostprint(&home, &["diff", "first"]));

    let dup = hostprint(&home, &["capture", "--name", "first"]);
    assert_eq!(dup.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&dup.stderr).contains("already exists"));

    assert_ok(&hostprint(&home, &["delete", "second"]));
    assert_eq!(hostprint(&home, &["diff", "first", "second"]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn secrets_never_reach_the_snapshot_file() {
    let home = temp_home("secrets");
    let env_file = home.with_extension("env");
    std::fs::write(&env_file, "STRIPE_SECRET_KEY=sk_live_abcdefghijklmnop\nPOOL_SIZE=20\n").unwrap();
    let out = hostprint_with_env(
        &home,
        &["capture", "--name", "s", "--env-file", env_file.to_str().unwrap()],
        &[("MY_API_TOKEN", "supersecretvalue123"), ("DATABASE_URL", "postgres://app:hunter2@db.internal/app")],
    );
    assert_ok(&out);
    let raw = std::fs::read_to_string(home.join("snapshots/s.hp")).unwrap();
    for secret in ["supersecretvalue123", "hunter2", "sk_live_abcdefghijklmnop"] {
        assert!(!raw.contains(secret), "{secret} leaked into the snapshot");
    }
    assert!(raw.contains("postgres://app:[REDACTED]@db.internal/app"));
    assert!(raw.contains("\"POOL_SIZE\""));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("snapshots/s.hp")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(&env_file);
}

#[test]
fn fail_on_sets_the_exit_code_and_files_can_be_compared() {
    let home = temp_home("failon");
    assert_ok(&hostprint(&home, &["capture", "--name", "base", "--quiet"]));
    let path = home.join("snapshots/base.hp");

    // Fabricate a changed snapshot file: a different kernel and a failed service.
    let mut snap: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    snap["name"] = "edited".into();
    snap["host"]["kernel"] = "99.0.0-test".into();
    let edited = home.join("edited.hp");
    std::fs::write(&edited, serde_json::to_vec(&snap).unwrap()).unwrap();
    let edited = edited.to_str().unwrap();

    assert_eq!(hostprint(&home, &["diff", "base", "base", "--fail-on", "low"]).status.code(), Some(0));
    let out = hostprint(&home, &["diff", "base", edited, "--fail-on", "medium"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(stdout(&out).contains("99.0.0-test"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn doctor_runs() {
    let home = temp_home("doctor");
    let out = hostprint(&home, &["doctor"]);
    assert_ok(&out);
    assert!(stdout(&out).contains("Hostprint Doctor"));
    let _ = std::fs::remove_dir_all(&home);
}
