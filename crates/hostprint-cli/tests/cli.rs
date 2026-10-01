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

/// Reads a .tar.gz into (path, contents) pairs.
fn read_bundle(path: &Path) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
    let file = std::fs::File::open(path).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    archive
        .entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let name = e.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            (name, data)
        })
        .collect()
}

#[test]
fn logs_are_collected_bounded_and_redacted() {
    let home = temp_home("logs");
    std::fs::create_dir_all(&home).unwrap();
    let log = home.join("app.log");
    let mut lines: String = (0..80).map(|i| format!("GET /health {i} 200\n")).collect();
    lines.push_str("ERROR payment failed: card declined token=tok_live_secret123\n");
    std::fs::write(&log, lines).unwrap();
    std::fs::write(home.join("config.toml"), format!("[logs]\nfiles = [\"{}\"]\nlines = 10\n", log.display())).unwrap();

    assert_ok(&hostprint(&home, &["capture", "--name", "l", "--logs-since", "10m", "--quiet"]));
    let raw = std::fs::read_to_string(home.join("snapshots/l.hp")).unwrap();
    assert!(!raw.contains("tok_live_secret123"), "secret in a log line leaked");
    let snap: Value = serde_json::from_str(&raw).unwrap();
    let source = snap["logs"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "file")
        .expect("file log source")
        .clone();
    assert_eq!(source["total"], 81);
    assert_eq!(source["errors"], 1);
    assert_eq!(source["lines"].as_array().unwrap().len(), 10, "kept lines are capped");
    assert_eq!(source["truncated"], true);
    assert!(source["lines"][9].as_str().unwrap().ends_with("token=[REDACTED]"));

    let show = hostprint(&home, &["show", "l", "--section", "logs"]);
    assert_ok(&show);
    assert!(stdout(&show).contains("81 lines · 1 error · 0 warnings"), "{}", stdout(&show));

    // Without --logs-since logs are off, and say so.
    assert_ok(&hostprint(&home, &["capture", "--name", "nologs", "--quiet"]));
    let snap: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("snapshots/nologs.hp")).unwrap()).unwrap();
    assert!(snap.get("logs").is_none());
    let report =
        snap["capture"]["collectors"].as_array().unwrap().iter().find(|c| c["name"] == "logs").unwrap().clone();
    assert_eq!(report["status"], "skipped");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn collectors_can_be_skipped() {
    let home = temp_home("skip");
    assert_ok(&hostprint(&home, &["capture", "--name", "s", "--skip", "runtimes,git", "--quiet"]));
    let snap: Value = serde_json::from_str(&std::fs::read_to_string(home.join("snapshots/s.hp")).unwrap()).unwrap();
    assert!(snap.get("runtimes").is_none());
    let status = |name: &str| {
        snap["capture"]["collectors"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap().clone()
    };
    assert_eq!(status("runtimes")["message"], "disabled");
    assert_eq!(status("system")["status"], "ok");
    let bad = hostprint(&home, &["capture", "--name", "t", "--only", "nope"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown collector 'nope'"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn bundles_contain_snapshot_diff_report_and_checksums() {
    let home = temp_home("bundle");
    assert_ok(&hostprint(&home, &["capture", "--name", "before", "--quiet"]));
    assert_ok(&hostprint(&home, &["capture", "--name", "after", "--quiet"]));
    let out_path = home.join("incident.tar.gz");
    let out = hostprint(&home, &["bundle", "after", "--against", "before", "-o", out_path.to_str().unwrap()]);
    assert_ok(&out);
    assert!(stdout(&out).contains("Bundle written"));

    let files = read_bundle(&out_path);
    let names: Vec<&str> = files.iter().map(|(n, _)| n.split_once('/').unwrap().1).collect();
    for expected in
        ["snapshot.json", "baseline.json", "diff.json", "report.md", "report.html", "manifest.json", "checksums.sha256"]
    {
        assert!(names.contains(&expected), "{expected} missing from {names:?}");
    }
    let get = |name: &str| &files.iter().find(|(n, _)| n.ends_with(&format!("/{name}"))).unwrap().1;
    let report = String::from_utf8_lossy(get("report.md"));
    assert!(report.contains("# Hostprint snapshot: `after`"));
    assert!(report.contains("# Hostprint diff: `before` → `after`"));

    // Every checksum matches its file.
    use sha2::Digest;
    let checksums = String::from_utf8_lossy(get("checksums.sha256")).into_owned();
    assert_eq!(checksums.lines().count(), files.len() - 1);
    for line in checksums.lines() {
        let (hash, name) = line.split_once("  ").unwrap();
        let actual: String = sha2::Sha256::digest(get(name)).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash, actual, "{name}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&out_path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Refuses to overwrite.
    let again = hostprint(&home, &["bundle", "after", "-o", out_path.to_str().unwrap()]);
    assert_eq!(again.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn baselines_check_and_export() {
    let home = temp_home("baseline");
    let created = hostprint(&home, &["baseline", "create", "production"]);
    assert_ok(&created);
    assert!(stdout(&created).contains("Baseline saved: production"));
    assert!(stdout(&hostprint(&home, &["baseline", "list"])).contains("production"));
    assert!(stdout(&hostprint(&home, &["list"])).contains("No snapshots"), "baselines are not snapshots");

    // Nothing significant changes between creating a baseline and checking it.
    let check = hostprint(&home, &["check", "production"]);
    assert_eq!(check.status.code(), Some(0), "{}", stdout(&check));
    assert!(stdout(&check).contains("Nothing at MEDIUM or above"));

    // A baseline from a doctored snapshot fails the check.
    assert_ok(&hostprint(&home, &["capture", "--name", "s", "--quiet"]));
    let path = home.join("snapshots/s.hp");
    let mut snap: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    snap["host"]["hostname"] = "some-other-host".into();
    let edited = home.join("edited.hp");
    std::fs::write(&edited, serde_json::to_vec(&snap).unwrap()).unwrap();
    assert_ok(&hostprint(&home, &["baseline", "create", "drifted", "--from", edited.to_str().unwrap()]));
    let check = hostprint(&home, &["check", "drifted", "--format", "markdown"]);
    assert_eq!(check.status.code(), Some(1));
    let md = stdout(&check);
    assert!(md.contains("## MEDIUM") && md.contains("`system.hostname`"), "{md}");

    let export = home.join("s.hostprint");
    assert_ok(&hostprint(&home, &["export", "s", "-o", export.to_str().unwrap()]));
    assert_ok(&hostprint(&home, &["diff", "s", export.to_str().unwrap(), "--fail-on", "low"]));
    assert_eq!(hostprint(&home, &["export", "s", "-o", export.to_str().unwrap()]).status.code(), Some(2));

    assert_ok(&hostprint(&home, &["baseline", "delete", "production"]));
    assert_eq!(hostprint(&home, &["check", "production"]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn html_and_markdown_reports() {
    let home = temp_home("report");
    assert_ok(&hostprint(&home, &["capture", "--name", "a", "--quiet"]));
    assert_ok(&hostprint(&home, &["capture", "--name", "b", "--quiet"]));

    let html_path = home.join("r.html");
    let out = hostprint(&home, &["report", "a", "b", "-o", html_path.to_str().unwrap()]);
    assert_ok(&out);
    let html = std::fs::read_to_string(&html_path).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<h1>a → b</h1>"), "{html}");
    assert!(html.contains("What changed") && html.contains("Collection"));
    assert!(!html.contains("<script") && !html.contains("http://") && !html.contains("https://"), "self-contained");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&html_path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Refuses to overwrite without --force.
    assert_eq!(hostprint(&home, &["report", "a", "b", "-o", html_path.to_str().unwrap()]).status.code(), Some(2));
    assert_ok(&hostprint(&home, &["report", "a", "b", "-o", html_path.to_str().unwrap(), "--force"]));

    let single = hostprint(&home, &["report", "a", "-o", "-"]);
    assert_ok(&single);
    assert!(stdout(&single).contains("<title>Hostprint: a</title>"));
    let md = hostprint(&home, &["report", "a", "b", "--format", "markdown", "-o", "-"]);
    assert!(stdout(&md).starts_with("# Hostprint snapshot: `b`"), "{}", stdout(&md));
    assert!(stdout(&md).contains("# Hostprint diff: `a` → `b`"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn interactive_commands_need_a_terminal() {
    let home = temp_home("tty");
    for args in [&["tui"][..], &["watch"][..]] {
        let out = hostprint(&home, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("needs an interactive terminal"));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn policies_adjust_levels_and_exit_codes() {
    let home = temp_home("policy");
    assert_ok(&hostprint(&home, &["capture", "--name", "s", "--quiet"]));
    let mut snap: Value = serde_json::from_str(&std::fs::read_to_string(home.join("snapshots/s.hp")).unwrap()).unwrap();
    snap["name"] = "edited".into();
    snap["host"]["hostname"] = "renamed-host".into(); // system.hostname: MEDIUM
    let edited = home.join("edited.hp");
    std::fs::write(&edited, serde_json::to_vec(&snap).unwrap()).unwrap();
    let edited = edited.to_str().unwrap();
    assert_eq!(hostprint(&home, &["diff", "s", edited, "--fail-on", "medium"]).status.code(), Some(1));

    // A policy file lowers it: the check passes, and the output says why.
    let file = home.join("team-policy.toml");
    std::fs::write(&file, "[[rules]]\nrule = \"system.hostname\"\nlevel = \"low\"\n").unwrap();
    let file = file.to_str().unwrap();
    let out = hostprint(&home, &["diff", "s", edited, "--fail-on", "medium", "--policy", file]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(stdout(&out).contains("[policy: was MEDIUM]"), "{}", stdout(&out));
    let json: Value =
        serde_json::from_slice(&hostprint(&home, &["--policy", file, "diff", "s", edited, "--json"]).stdout).unwrap();
    let change = &json["changes"][0];
    assert_eq!((change["significance"].as_str(), change["policy"]["default"].as_str()), (Some("low"), Some("medium")));

    // config.toml turns system.* off; the --policy file still wins for the hostname.
    std::fs::write(home.join("config.toml"), "[[policy.rules]]\nrule = \"system.*\"\nlevel = \"off\"\n\n[[policy.rules]]\nrule = \"sytem.kernel\"\nlevel = \"high\"\n").unwrap();
    let out = hostprint(&home, &["diff", "s", edited]);
    assert!(stdout(&out).contains("Policy turned off 1 change: system.hostname ×1."), "{}", stdout(&out));
    assert!(stdout(&hostprint(&home, &["diff", "s", edited, "--policy", file])).contains("[policy: was MEDIUM]"));

    // `policy show` explains the effective policy and flags the typo.
    let show = hostprint(&home, &["policy", "show", "--policy", file]);
    assert_ok(&show);
    let text = stdout(&show);
    assert!(text.contains("system.hostname → LOW"), "{text}");
    assert!(text.contains("sytem.kernel → HIGH") && text.contains("matches no rule"), "{text}");
    assert!(stdout(&hostprint(&home, &["policy", "rules"])).contains("container.health"));

    // Invalid levels and thresholds are errors, not silently ignored.
    std::fs::write(home.join("config.toml"), "[[policy.rules]]\nrule = \"git.*\"\nlevel = \"severe\"\n").unwrap();
    let bad = hostprint(&home, &["diff", "s", edited]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown level 'severe'"));
    std::fs::write(home.join("config.toml"), "[policy.thresholds]\ndisk_high_percent = 150\n").unwrap();
    assert_eq!(hostprint(&home, &["diff", "s", edited]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&home);
}

/// Regression: `hostprint policy rules | head` used to panic with "Broken pipe".
#[test]
fn a_closed_pipe_ends_quietly() {
    let home = temp_home("pipe");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hostprint"))
        .args(["policy", "rules"])
        .env("HOSTPRINT_HOME", &home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Close the reading end before the first write, as an early-exiting
    // `head` would, so every write fails.
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_ne!(out.status.code(), Some(101), "exit status of a Rust panic");
}
