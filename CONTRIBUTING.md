# Contributing to Hostprint

Thanks for helping. Bug reports with a snapshot diff attached (`hostprint diff
a b --json`, reviewed for anything sensitive) are especially useful: most
improvements to Hostprint are improvements to its noise reduction.

## Building and testing

Hostprint is a Rust workspace. v0.1 targets Linux; the code compiles
elsewhere, but collectors report "not supported" and the end-to-end tests
only run on Linux.

```sh
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

CI runs all four. On macOS or Windows, test inside a Linux container:

```sh
docker run --rm -v "$PWD:/src" -w /src rust:1 cargo test --workspace
```

To try the binary against a real systemd, a container such as
`jrei/systemd-ubuntu` started with `--privileged --cgroupns=host -v
/sys/fs/cgroup:/sys/fs/cgroup:rw` works well. `examples/demo` exercises the
Docker collector and the diff end to end.

## Layout

```text
crates/
  hostprint-model/       Snapshot types; their JSON *is* the file format
  hostprint-collectors/  One module per collector, plus redaction
  hostprint-core/        Capture engine (parallel collectors) and config.toml
  hostprint-storage/     ~/.hostprint: snapshots, permissions, fingerprint key
  hostprint-diff/        Comparison rules and significance
  hostprint-cli/         The `hostprint` binary: argument parsing and rendering
docs/
  diff-rules.md          Every rule and its threshold
  snapshot-format.md     The documented, versioned file format
examples/demo/           Reproducible incident for the README
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
   and pass any value that could hold a secret through `ctx.redactor`.
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
