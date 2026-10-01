# Changelog

## [Unreleased]

### Added

- `hostprint capture`: point-in-time snapshot of a Linux machine (system,
  resources and pressure, processes, listening sockets and network, systemd
  services, Docker containers, Git state, runtime versions, environment and
  dotenv variables, file fingerprints). Collectors run in parallel; one
  failing, panicking or hanging never fails or stalls the capture.
- Secret redaction by variable name and by value shape, with keyed
  fingerprints so changed secrets are detected without being stored.
- `hostprint diff`: deterministic, rule-based comparison with HIGH / MEDIUM /
  LOW / INFO significance, noise reduction, `--json` output and `--fail-on`.
  Omitting the second snapshot compares against the live system.
- `hostprint list`, `show` (with `--section` and `--json`), `delete`, `doctor`.
- Snapshot storage in `~/.hostprint` (or `$HOSTPRINT_HOME`) with `0700`/`0600`
  permissions and a versioned JSON format.
- Log collection with `--logs-since` or `collect_logs = true`: systemd journal
  (warning and worse), Docker container output and configured log files.
  Bounded, and every line redacted. Diffs report error bursts (`log.errors`)
  and new error messages (`log.new_error`); `show --section logs` prints the
  lines.
- Free-text redaction for log lines and command lines: `key=value`,
  `"key": "value"`, `Authorization: Bearer …`, credential URLs and tokens,
  including secrets inside inline scripts (`sh -c '…'`).
- `hostprint bundle`: a `.tar.gz` with the snapshot, optional baseline and
  diff, a Markdown report, logs, a manifest and `sha256sum`-compatible
  checksums. Without a snapshot argument it captures first, as a support
  bundle.
- `hostprint baseline create|list|show|delete` and `hostprint check`, which
  compares the live system with a baseline and exits 1 at MEDIUM or above by
  default.
- `hostprint export` writes `<name>.hostprint` for sharing.
- `--format markdown` for `diff` and `check`.
- Collector selection: `--only`, `--skip`, and `[collectors] disable`.
- Docker-based development without a local Rust toolchain:
  `scripts/dev.ps1` / `scripts/dev.sh` (`check`, `test`, `build`, `run`,
  `demo`, `shell`) and a root `Dockerfile` that outputs the static binary.
- Reproducible Docker demo in `examples/demo`, now with logs and a bundle
  (`OUT=dir` keeps the bundle).
- `hostprint tui`: browse snapshots and baselines, inspect a snapshot's
  processes, ports, services, containers, disks, environment and logs with a
  filter, compare two snapshots (or one with the live system), filter the
  diff by level and category, inspect a change's rule and values, and write a
  bundle.
- `hostprint watch`: a live dashboard that captures on an interval and shows
  resource gauges, service and container health (problems first), drift from
  the first capture or a `--baseline`, and a timeline of state changes.
- `hostprint report`: a standalone HTML report (inline CSS, no scripts, light
  and dark) of a snapshot or of a comparison; `--format markdown` too.
  Bundles now include `report.html`.
- The terminal UI is behind the default `tui` cargo feature.
- Diff policies: `[[policy.rules]]` in `config.toml` (or a `--policy` file)
  set a rule's level or turn it off, by rule-id glob and optionally subject
  glob; `[policy.thresholds]` moves the disk, memory and load limits. Adjusted
  changes record their default level and the policy rule (`policy` in JSON,
  marked in every output); changes turned off are counted in a note.
- `hostprint policy show` (effective policy, warnings for entries matching no
  rule) and `hostprint policy rules` (every rule id).

- Capture over SSH: `hostprint capture ssh://[user@]host[:port]`, and
  `ssh://` on either side of `diff` (`hostprint diff ssh://web-1
  ssh://web-2`). Uses the system ssh client, streams the binary to a private
  temporary directory and removes it afterwards; per-host fingerprint keys
  keep secret changes comparable between captures of the same host.
  `--remote-binary` sends a different build. Snapshots record
  `capture.remote`.
- `HOSTPRINT_*` variables are INFO in diffs.

### Fixed

- Piping output into a command that exits early (`hostprint list | head`)
  no longer panics with "Broken pipe".

### Changed

- Terminal and shell variables (`TERM`, `TERM_PROGRAM`, `COLORTERM`, `SHELL`)
  are INFO in diffs: they describe where Hostprint was started from, not the
  system.
- `process.uninterruptible` and `process.zombies` need a real jump (×3 and
  ×2) and count only processes older than a minute, so ordinary I/O waits on
  a busy machine no longer register.
