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

  healthy  2026-10-01 14:36:09 UTC  df46967e8875
→ broken   2026-10-01 14:36:46 UTC  df46967e8875  (36s later)

10 changes: 3 high · 3 medium · 4 low

HIGH
  CONTAINERS
    hostprint-demo-redis-1  health         healthy → unhealthy
    hostprint-demo-redis-1  restart count  0 → 7  (+7)
    hostprint-demo-redis-1  state          running → restarting

MEDIUM
  APPLICATION
    app                        commit  645cb65 Release 1.4.0 → 7d9cf16 Raise DATABASE_POOL_SIZE to 50
  CONFIGURATION
    DATABASE_POOL_SIZE (.env)  value   10 → 50
    JWT_SECRET (.env)          secret  fp 247db897 → fp 1671d955  (value redacted)

LOW
  RESOURCES
    Memory available                      2.8 GiB → 1.7 GiB  (-38%, 47% of total)
  CONTAINERS
    hostprint-demo-api-1    container ID  f2f5e517dcd0 → 90ccd09a3fe2
    hostprint-demo-hog-1    container     + running · python:3.13-alpine
    hostprint-demo-redis-1  container ID  a47ffaa16467 → 5c0350369650
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

A capture typically takes under a second (collectors run in parallel), and a
snapshot is tens to hundreds of kilobytes.

## Install

Hostprint v0.1 supports Linux. It is tested on x86_64; ARM64 builds from the
same code but has not been tested yet. From source, with a Rust toolchain:

```sh
git clone <this repository> hostprint
cd hostprint
cargo install --path crates/hostprint-cli
```

The release build is a single binary of about 3 MB; built for
`x86_64-unknown-linux-musl` it is fully static. There are no published
releases yet. `.github/workflows/release.yml` is set up to attach static
x86_64 and ARM64 binaries to a GitHub Release when a `v*` tag is pushed.

Run `hostprint doctor` to see what it can observe on your machine.

## Usage

```text
hostprint capture [--name NAME] [--repo DIR] [--env-file FILE] [--file PATH] [--json] [--force]
hostprint list
hostprint show NAME [--section processes|ports|interfaces|disks|services|containers|env|runtimes|files|collectors] [--json]
hostprint diff FROM [TO] [--all] [--min LEVEL] [--fail-on LEVEL] [--json]
hostprint delete NAME
hostprint doctor
```

- **Compare against now.** `hostprint diff healthy` captures the current state
  (without saving it) and compares.
- **Compare across machines.** Snapshot names and file paths are
  interchangeable: `hostprint diff healthy ./web-2.hp`.
- **Script it.** `--json` on `capture`, `show`, `list` and `diff` emits
  machine-readable output; `diff --fail-on medium` exits with status 1 when
  something at or above MEDIUM changed, for CI and health checks.
- **Record your application.** `--repo` points the Git collector at your
  deployment, `--env-file` records a dotenv file, `--file` fingerprints a
  config file. All three can be set permanently in `config.toml`.

Exit status: `0` success, `1` `--fail-on` threshold met, `2` error.

## How changes are classified

Every difference goes through a deterministic rule that assigns HIGH, MEDIUM,
LOW or INFO, and the rule's id is included in `--json` output. Values that
change constantly (PIDs, uptime, timestamps, CPU percentages, small memory and
disk fluctuations, ephemeral ports, virtual interfaces, short-lived and
interactive processes, terminal variables) are suppressed or demoted to INFO,
so two captures of an unchanged machine produce no differences.

Hostprint shows evidence ("restart count 0 → 7"), not conclusions. The full
rule list with every threshold is in [docs/diff-rules.md](docs/diff-rules.md).

## Privacy and secrets

- Secrets are redacted **before** anything is stored: by name (`*PASSWORD*`,
  `*SECRET*`, `*TOKEN*`, `*KEY*`, ...), and by shape (URLs with passwords,
  `ghp_…`/`sk_live_…`/`AKIA…` tokens, JWTs, PEM blocks, long random strings).
  This covers environment variables, dotenv files and process command lines.
- Redacted values keep a keyed fingerprint (HMAC-SHA256 with a per-install
  key), so a diff can say *a secret changed* without anyone being able to
  recover or brute-force it from the snapshot.
- Hostprint records metadata, not content: hashes of config files, names of
  changed files, never source code or file contents.
- Snapshots are stored in `~/.hostprint` with `0700`/`0600` permissions.
  Nothing leaves the machine unless you copy it.
- Root is never required. Without it, other users' process details are
  partial, and the capture says so.

Details and limits are in [SECURITY.md](SECURITY.md).

## Configuration

`~/.hostprint/config.toml` (or `$HOSTPRINT_HOME/config.toml`) is optional:

```toml
[hostprint]
redact_secrets = true

[files]
paths = ["/etc/nginx/nginx.conf", "/etc/myapp/config.toml"]

[env]
capture_process = true
files = ["/srv/myapp/.env"]

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

v0.1 (this release) covers the core loop on Linux: capture, list, show, diff,
doctor, JSON output and secret redaction.

Planned next:

- **v0.2:** incident bundles (`hostprint bundle`), system and Docker log
  collection, Markdown reports, baselines (`hostprint baseline` /
  `hostprint check`), configurable collectors.
- **v0.3:** watch mode and a terminal UI, custom diff policies, macOS
  support, capture over SSH.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workspace layout, how to add a
collector or a diff rule, and how to test on non-Linux machines.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.
