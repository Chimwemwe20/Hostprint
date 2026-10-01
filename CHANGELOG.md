# Changelog

## [Unreleased]

### Added

- `hostprint capture`: point-in-time snapshot of a Linux machine (system,
  resources and pressure, processes, listening sockets and network, systemd
  services, Docker containers, Git state, runtime versions, environment and
  dotenv variables, file fingerprints). Collectors run in parallel; one failing
  never fails the capture.
- Secret redaction by variable name and by value shape, with keyed fingerprints
  so changed secrets are detected without being stored.
- `hostprint diff`: deterministic, rule-based comparison with HIGH / MEDIUM /
  LOW / INFO significance, noise reduction, `--json` output and `--fail-on`.
  Omitting the second snapshot compares against the live system.
- `hostprint list`, `show` (with `--section` and `--json`), `delete`, `doctor`.
- Snapshot storage in `~/.hostprint` (or `$HOSTPRINT_HOME`) with `0700`/`0600`
  permissions and a versioned JSON format.
- Reproducible Docker demo in `examples/demo`.
