# Hostprint

**Fingerprint your system. See what changed.**

```sh
hostprint capture --name healthy

# later, when something is wrong...

hostprint capture --name broken

hostprint diff healthy broken
```

```text
HOSTPRINT DIFF

  healthy  2026-10-01 16:49:24 UTC  adf5dcdb295c
→ broken   2026-10-01 16:50:01 UTC  adf5dcdb295c  (37s later)

12 changes: 3 high · 4 medium · 5 low

HIGH
  CONTAINERS
    hostprint-demo-redis-1  health         healthy → unhealthy
    hostprint-demo-redis-1  restart count  0 → 8  (+8)
    hostprint-demo-redis-1  state          running → restarting

MEDIUM
  APPLICATION
    app                        commit         507c111 Release 1.4.0 → 497bb90 Raise DATABASE_POOL_SIZE to 50
  CONFIGURATION
    DATABASE_POOL_SIZE (.env)  value          10 → 50
    JWT_SECRET (.env)          secret         fp d3129725 → fp 9e9e38a1  (value redacted)
  LOGS
    hostprint-demo-redis-1     docker errors  0 → 8  (+8, last 10m)

LOW
  RESOURCES
    Memory available                          2.8 GiB → 1.7 GiB  (-38%, 47% of total)
  CONTAINERS
    hostprint-demo-api-1    container ID      0977b9201a98 → 830e201f9d45
    hostprint-demo-hog-1    container         + running · python:3.13-alpine
    hostprint-demo-redis-1  container ID      e07bf372278c → 61b4144e6a6d
  LOGS
    hostprint-demo-redis-1  docker new error  + FATAL: cannot open append-only file  (×8)
```

This is unedited output from [`examples/demo`](examples/demo), a
reproducible incident you can run yourself (the hostname is the container
Hostprint ran in).

Hostprint records the state of a machine at a point in time and compares it
with another point in time. It answers one question: **what is different now
compared with when this system was healthy?**

It is a single local binary. No account, no server, no agent, no network
calls, no telemetry. It is not an APM, a log platform or a monitor; it creates
point-in-time evidence and makes that evidence comparable.

## What it captures

| Area           | Recorded |
| -------------- | -------- |
| System         | Hostname, OS, kernel, architecture, boot time, timezone, virtualization |
| Resources      | CPU, load, memory, swap, pressure stall (PSI), disk and inode usage, read-only and hung mounts |
| Processes      | Name, PID, parent, user, state, resident memory, CPU, start time, redacted command line |
| Network        | Interfaces and addresses, listening TCP/UDP sockets and their processes, TCP state counts, default routes, DNS |
| Services       | systemd units: state, sub-state, type, automatic restart count |
| Containers     | Docker: state, health, restart count, exit code, OOM kills, image and image ID, ports, memory |
| Application    | Git branch, commit, dirty state and changed file names; versions of Node.js, Python, Java, Go, Rust, Ruby, PHP, .NET, Deno, Bun, Elixir, Erlang, Docker, PostgreSQL, MySQL, Redis, nginx, OpenSSL, Git |
| Configuration  | Environment variables and dotenv files, with secrets redacted |
| Files          | Size, mtime, mode, owner and SHA-256 of files you choose |
| Logs (opt-in)  | Recent systemd journal warnings and errors, Docker container output, log files you choose; redacted and bounded |

A capture typically takes under a second (collectors run in parallel), and a
snapshot is tens to hundreds of kilobytes.

## Install

Hostprint supports Linux. It is tested on x86_64; ARM64 builds from the same
code but has not been tested yet.

**With Docker, no Rust needed.** This builds a static binary into `dist/`:

```sh
git clone <this repository> hostprint && cd hostprint
docker build --target binary --output dist .
sudo install dist/hostprint /usr/local/bin/
```

**With a Rust toolchain:**

```sh
cargo install --path crates/hostprint-cli
```

The binary is about 4 MB and, built for musl as above, fully static. Building
with `--no-default-features` leaves out the terminal UI. There are no
published releases yet; `.github/workflows/release.yml` is set up to
attach static x86_64 and ARM64 binaries to a GitHub Release when a `v*` tag is
pushed.

Run `hostprint doctor` to see what it can observe on your machine.

## Usage

```text
hostprint capture  [--name NAME] [--logs-since 30m] [--repo DIR] [--env-file FILE] [--file PATH]
                   [--only LIST] [--skip LIST] [--json] [--force]
hostprint list
hostprint show     NAME [--section processes|ports|interfaces|disks|services|containers|env|runtimes|files|logs|collectors] [--json]
hostprint diff     FROM [TO] [--format text|json|markdown] [--all] [--min LEVEL] [--fail-on LEVEL]
hostprint tui
hostprint watch    [--interval 10s] [--baseline NAME]
hostprint report   SNAPSHOT [TO] [--format html|markdown] [--output FILE]
hostprint bundle   [SNAPSHOT] [--against SNAPSHOT] [--output FILE]
hostprint baseline create NAME [--from SNAPSHOT] | list | show NAME | delete NAME
hostprint check    BASELINE [--format ...] [--fail-on LEVEL]
hostprint export   NAME [--output FILE]
hostprint delete   NAME
hostprint doctor
```

- **Compare against now.** `hostprint diff healthy` captures the current state
  (without saving it) and compares.
- **Compare across machines.** Snapshot names and file paths are
  interchangeable. `hostprint export web-2` writes `web-2.hostprint`, which
  anyone can pass to `diff` or `show`.
- **Include logs.** `--logs-since 30m` adds recent journal warnings and
  errors, container output and configured log files. The diff then reports
  error bursts and error messages that weren't there before.
- **Script it.** `--json` everywhere, `--format markdown` for tickets and pull
  requests, and `--fail-on medium` exits with status 1 when something at or
  above MEDIUM changed.
- **Record your application.** `--repo` points the Git collector at your
  deployment, `--env-file` records a dotenv file, `--file` fingerprints a
  config file. All of these can be set permanently in `config.toml`.

Exit status: `0` success, `1` threshold met (`--fail-on`, or `check`'s default
of MEDIUM), `2` error.

### Terminal UI

`hostprint tui` lets you browse snapshots and baselines; open one to page
through its processes, ports, services, containers, disks, environment and
logs, with `/` to filter. Mark one with Space and press `c` to compare (or
`n` to compare it with the system as it is now). The diff view filters by
level (`m`) and category (`c`), Enter shows a change's rule and full values,
and `b` writes an incident bundle.

`hostprint watch` is a live dashboard. It captures every 10 seconds (or
`--interval`). Here it is rendered from the test fixtures, where Redis starts
crash-looping and nginx fails between two captures (blank rows trimmed):

```text
 HOSTPRINT  WATCH web-1 · every 10s · capture #2 · paused
┌ CPU ──────────────────┐┌ Memory ───────────────┐┌ Disk ─────────────────┐┌ Load ─────────────────┐
│█████████ 38%          ││████38% of 8.0 GiB     ││█████████/ 54%         ││██  0.42 (0.1/core)    │
└───────────────────────┘└───────────────────────┘└───────────────────────┘└───────────────────────┘
┌ Services · 0 active · 1 failed ────────────────┐┌ Containers · 1/2 running ──────────────────────┐
│nginx                  failed (failed)  ↻3      ││redis                  restarting unhealthy  ↻17│
│                                                ││api                    running healthy          │
└────────────────────────────────────────────────┘└────────────────────────────────────────────────┘
┌ Changed since first capture at 14:13:20 · 4 high · 1 medium · 0 low ─────────────────────────────┐
│HIGH   SERVICES      nginx.service               state              active (running) → failed (fai│
│HIGH   CONTAINERS    redis                       health             healthy → unhealthy           │
│HIGH   CONTAINERS    redis                       restart count      0 → 17                        │
│HIGH   CONTAINERS    redis                       state              running → restarting          │
│MEDIUM SERVICES      nginx.service               automatic restarts 0 → 3                         │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Recent state changes ────────────────────────────────────────────────────────────────────────────┐
│14:13:30 HIGH   nginx.service state active (running) → failed (failed)                            │
│14:13:30 HIGH   redis health healthy → unhealthy                                                  │
│14:13:30 HIGH   redis restart count 0 → 17                                                        │
│14:13:30 HIGH   redis state running → restarting                                                  │
│14:13:30 MEDIUM nginx.service automatic restarts 0 → 3                                            │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 space capture now  p resume  r reset reference  s save snapshot  ? help  q quit
```

Problems are listed first. The timeline shows what changed between consecutive
captures, and `--baseline production` measures drift from a baseline instead
of from the first capture. Both views run in any terminal, including over SSH.

### HTML reports

```sh
hostprint report healthy broken     # writes healthy-to-broken.html
hostprint report broken             # one snapshot
```

The report is a single file with inline styles, no scripts and no external
resources, so it opens the same from an email attachment or a ticket. It
follows the reader's light or dark preference. Bundles include it as
`report.html`.

### Baselines and checks

Record a known-good state once, then compare against it whenever you need to:

```sh
hostprint baseline create production --logs-since 30m
hostprint check production          # exits 1 if anything at MEDIUM or above changed
```

### Incident bundles

```sh
hostprint bundle broken --against healthy
```

writes `broken-20261001-165001.tar.gz` with `snapshot.json`, `baseline.json`,
`diff.json`, `report.md` and `report.html`, the collected `logs/`, a `manifest.json`,
and a `checksums.sha256` you can verify with `sha256sum -c`. It is ready to
attach to a GitHub issue or support ticket. With no snapshot named,
`hostprint bundle` captures the system first, which makes it a one-command
support bundle.

## How changes are classified

Every difference goes through a deterministic rule that assigns HIGH, MEDIUM,
LOW or INFO, and the rule's id is included in `--json` and Markdown output.
Values that change constantly (PIDs, uptime, timestamps, CPU percentages,
small memory and disk fluctuations, ephemeral ports, virtual interfaces,
short-lived and interactive processes, terminal variables) are suppressed or
demoted to INFO, so two captures of an unchanged machine produce no
differences.

Hostprint shows evidence ("restart count 0 → 8"), not conclusions. The full
rule list with every threshold is in [docs/diff-rules.md](docs/diff-rules.md).

## Privacy and secrets

- Secrets are redacted **before** anything is stored: by name (`*PASSWORD*`,
  `*SECRET*`, `*TOKEN*`, `*KEY*`, ...), and by shape (URLs with passwords,
  `ghp_…`/`sk_live_…`/`AKIA…` tokens, JWTs, PEM blocks, long random strings).
  This covers environment variables, dotenv files, process command lines
  (including inline scripts) and every collected log line.
- Redacted values keep a keyed fingerprint (HMAC-SHA256 with a per-install
  key), so a diff can say *a secret changed* without anyone being able to
  recover or brute-force it from the snapshot.
- Hostprint records metadata by default: hashes of config files, names of
  changed files, never source code. Logs, the one kind of content, are
  opt-in and bounded.
- Snapshots, baselines, exports and bundles are written with `0600`
  permissions. Nothing leaves the machine unless you copy it.
- Root is never required. Without it, other users' process details and the
  system journal are partial, and the capture says so.

Details and limits are in [SECURITY.md](SECURITY.md).

## Configuration

`~/.hostprint/config.toml` (or `$HOSTPRINT_HOME/config.toml`) is optional:

```toml
[hostprint]
redact_secrets = true
collect_logs = false         # true: every capture includes [logs]

[collectors]
disable = ["runtimes"]       # turn collectors off

[files]
paths = ["/etc/nginx/nginx.conf", "/etc/myapp/config.toml"]

[env]
capture_process = true
files = ["/srv/myapp/.env"]

[logs]
since = "30m"
journal = true
docker = true
files = ["/var/log/myapp/app.log"]
lines = 50                   # kept per source

[redact]
patterns = ["INTERNAL_"]     # extra name fragments to treat as secret
allow = ["SSH_AUTH_SOCK"]    # names never redacted by name

[ignore]                     # left out of diffs; * wildcards allowed
processes = ["chrome"]
ports = [5353]
env = ["BUILD_*"]
containers = ["buildkit*"]
services = ["apt-daily*"]
```

## Snapshot format

Snapshots are versioned, documented JSON (`schemaVersion: 1`). Sections that
could not be collected are recorded as such, with the reason, instead of
silently appearing empty. See [docs/snapshot-format.md](docs/snapshot-format.md).

## Status and roadmap

Done:

- **v0.1:** capture, list, show, diff, doctor, JSON output, secret redaction.
- **v0.2:** incident bundles, journal / Docker / file log collection,
  Markdown reports, baselines and `check`, configurable collectors, export.
- **v0.3 (part):** terminal UI (`tui`), live dashboard (`watch`), standalone
  HTML reports.

Next: custom diff policies, macOS support, capture over SSH.

## Developing

No local Rust toolchain is needed. `scripts/dev.ps1` (Windows) and
`scripts/dev.sh` (Linux, macOS) run everything in Docker:

```sh
./scripts/dev.sh check                  # fmt, clippy and tests, as CI runs them
./scripts/dev.sh run capture --name x   # try the CLI
./scripts/dev.sh build                  # static binary in dist/
./scripts/dev.sh demo                   # the README demo, against your Docker
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workspace layout and how to add
a collector or a diff rule.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.
