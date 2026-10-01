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
- Reproducible Docker demo in `examples/demo`, now with logs and a bundle.
