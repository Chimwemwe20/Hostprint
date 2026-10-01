# Contributing to Hostprint

Thanks for helping. Bug reports with a snapshot diff attached (`hostprint diff
a b --json`, reviewed for anything sensitive) are especially useful: most
improvements to Hostprint are improvements to its noise reduction.

## Building and testing

Hostprint is a Rust workspace that targets Linux. The code compiles
elsewhere, but collectors report "not supported" and the end-to-end tests
only run on Linux.

### With Docker (no Rust install, any OS)

`scripts/dev.ps1` (Windows PowerShell) and `scripts/dev.sh` (Linux, macOS)
run everything in a toolchain container. The `hostprint-dev` image is built
on first use; Cargo's cache and build output live in Docker volumes, so later
runs are fast.

| Command              | Does |
| -------------------- | ---- |
| `check`              | `cargo fmt --check`, clippy with `-D warnings`, all tests: exactly what CI runs |
| `test [args]`        | `cargo test --workspace [args]` |
| `fmt`                | `cargo fmt --all` |
| `build`              | static release binary in `dist/hostprint` |
| `run <args>`         | the CLI, e.g. `run capture --name x`; snapshots persist in the `hostprint-home` volume and the Docker socket is mounted |
| `demo`               | builds, then runs `examples/demo` against your Docker |
| `shell`              | interactive shell in the toolchain container |
| `cargo <args>`       | any cargo command |
| `clean`              | removes the image and the cache volumes |

```powershell
.\scripts\dev.ps1 check
.\scripts\dev.ps1 run diff healthy
```

Note that `run` executes inside a container, so it sees the container's
processes and network rather than your machine's. That is fine for trying the
CLI. To observe a real Linux host, run the `dist/hostprint` binary on it.

### With a local Rust toolchain

```sh
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

### Testing against real systems

- **systemd and the journal:** a container such as `jrei/systemd-ubuntu`
  started with `--privileged --cgroupns=host -v
  /sys/fs/cgroup:/sys/fs/cgroup:rw` runs a real systemd. Copy
  `dist/hostprint` in with `docker cp` and exercise the services and journal
  collectors.
- **Docker:** `dev demo` runs a stack, breaks it and diffs it; any container
  with `/var/run/docker.sock` mounted can run the Docker collector.

## Layout

```text
crates/
  hostprint-model/       Snapshot types; their JSON *is* the file format
  hostprint-collectors/  One module per collector, plus redaction
  hostprint-core/        Capture engine (parallel collectors) and config.toml
  hostprint-storage/     ~/.hostprint: snapshots, permissions, fingerprint key
  hostprint-diff/        Comparison rules and significance
  hostprint-cli/         The `hostprint` binary: arguments, rendering, Markdown
                         reports, bundles, baselines
docs/
  diff-rules.md          Every rule and its threshold
  snapshot-format.md     The documented, versioned file format
docker/dev.Dockerfile    Toolchain image used by scripts/dev.*
Dockerfile               Builds the static release binary (no Rust needed)
examples/demo/           Reproducible incident for the README
scripts/dev.ps1, dev.sh  Docker-based build, test and run
```

Dependencies flow one way: `model` ← `collectors` ← `core` ← `cli`, with
`diff` and `storage` depending only on `model`. Keep dependencies few; every
crate added ends up in a binary people run on production machines.

## Adding a collector

1. Add a section type to `hostprint-model` (optional fields, camelCase, a doc
   comment for every non-obvious field) and an `Option<...>` field on
   `Snapshot`.
2. Write the collector in `hostprint-collectors/src/<name>.rs`. Read system
   files through `ctx.path("/proc/...")` so tests can use a fixture tree, run
   commands through `run_command` (it has a timeout and a pinned environment),
   and pass any value that could hold a secret through `ctx.redactor`:
   `pair` for name/value settings, `args` for command lines, `text` for log
   lines and other free text.
3. Return `CollectError::Unavailable` when the collector doesn't apply here and
   `CollectError::Failed` when it should have worked; add `notes` for partial
   results. Never panic, never block without a timeout.
4. Register it in `default_collectors()` and map its `Section` in
   `hostprint-core/src/engine.rs`. Sort any lists in `normalize`.
5. Put parsing in pure functions and unit-test them with real command output.

## Adding or changing a diff rule

Rules live in `hostprint-diff/src/<category>.rs`. Each change needs:

- a stable `rule` id (`category.what`) and `key` (`section/subject/field`);
- a significance that matches the definitions in `docs/diff-rules.md`;
- a test in the same file, written against `testing::baseline()`: clone it,
  change one thing, assert the exact `(rule, significance)` list;
- an entry in `docs/diff-rules.md`.

Before raising a significance, ask whether the change could appear between
two captures of a healthy machine. If it could, it needs a threshold, or it
belongs at INFO. The `identical_snapshots_have_no_changes` test and the
back-to-back capture test in `hostprint-cli/tests/cli.rs` guard this.

## Snapshot format changes

Additive, optional fields are fine within a schema version. Anything else
bumps `SCHEMA_VERSION` in `hostprint-model` and needs a note in
`docs/snapshot-format.md`. Older snapshots must keep loading.

## Style

`rustfmt.toml` sets the format. Prefer small pure functions over clever ones,
comment the *why* (thresholds especially), and keep user-facing text
evidence-based: say what changed, not what caused the incident.
