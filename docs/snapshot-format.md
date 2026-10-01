# Snapshot format

A snapshot is one UTF-8 JSON document. Stored snapshots live in
`~/.hostprint/snapshots/<name>.hp` (or under `$HOSTPRINT_HOME`), pretty-printed
so they diff and review well in plain tools. `hostprint show <name> --json`
prints the same document; `hostprint capture --json` prints it on stdout.

Baselines (`hostprint baseline create`) use the same format and live in
`~/.hostprint/baselines/<name>.hp`. `hostprint export <name>` writes a copy as
`<name>.hostprint` for sharing.

Any `.hp`, `.hostprint` or `.json` snapshot file can be passed wherever a
snapshot name is accepted, so snapshots can be copied between machines and
compared:

```sh
hostprint diff healthy ./web-2.hostprint
```

Incident bundles (`hostprint bundle`) are `.tar.gz` archives with one
top-level directory containing `snapshot.json` (this format), optionally
`baseline.json` and `diff.json`, `report.md`, `report.html`, `logs/<kind>-<source>.log`,
`manifest.json` (`bundleVersion: 1`, the snapshot references and every file's
size and SHA-256), and `checksums.sha256` in `sha256sum -c` format.

## Versioning

Every snapshot has a top-level `schemaVersion` (currently `1`).

- Within a schema version, changes are additive only: new optional fields,
  new sections. Older Hostprint builds ignore fields they do not know.
- Anything else (renaming, removing or changing the meaning of a field) bumps
  `schemaVersion`.
- Hostprint reads every schema version up to its own, and refuses newer ones
  with a clear error rather than misreading them.

Keys are camelCase. Optional values are omitted rather than `null`.

## Top level

| Field           | Type   | Description |
| --------------- | ------ | ----------- |
| `schemaVersion` | number | Format version |
| `id`            | string | `snap_` + a ULID; sorts by capture time |
| `name`          | string | Snapshot name |
| `capturedAt`    | string | RFC 3339 UTC timestamp |
| `capture`       | object | How the snapshot was taken (below) |
| `host`, `resources`, `processes`, `network`, `services`, `docker`, `git`, `runtimes`, `environment`, `files`, `logs` | object / array | One section per collector |

A missing section means it was **not collected**, which is different from an
empty one. `capture.collectors` says why.

## `capture`

```json
{
  "hostprintVersion": "0.1.0",
  "durationMs": 312,
  "user": "deploy",
  "uid": 1000,
  "elevated": false,
  "workingDir": "/srv/app",
  "remote": "ssh://deploy@web-1",
  "collectors": [
    { "name": "processes", "status": "partial", "durationMs": 268, "summary": "412 processes",
      "notes": ["executable paths unavailable for 37 processes owned by other users (run as root for full details)"] },
    { "name": "docker", "status": "failed", "durationMs": 1, "message": "Docker daemon not reachable at /var/run/docker.sock" },
    { "name": "git", "status": "skipped", "durationMs": 3, "message": "/root is not inside a Git repository" }
  ]
}
```

`remote` is present only for snapshots captured over SSH (`capture ssh://…`);
everything else describes the remote machine as it saw itself.

`status` is one of:

| Status    | Section present | Meaning |
| --------- | --------------- | ------- |
| `ok`      | yes | Collected completely |
| `partial` | yes | Collected with caveats in `notes`, usually missing permissions |
| `skipped` | no  | Not applicable here (tool not installed, not a repository, nothing configured) |
| `failed`  | no  | Applicable but could not be collected; see `message` |

## Sections

| Section       | Collector     | Contents |
| ------------- | ------------- | -------- |
| `host`        | `system`      | `hostname`, `os` (`id`, `name`, `versionId`, `prettyName`), `kernel` (release), `kernelName` (`Linux` or `Darwin`; absent in older snapshots, which are Linux), `architecture`, `bootTime`, `uptimeSeconds`, `timezone`, `hardware`, `container` |
| `resources`   | `resources`   | `cpu` (`model`, `logicalCores`, `physicalCores`, sampled `usagePercent` / `iowaitPercent` / `stealPercent`), `load` (`one`, `five`, `fifteen`), `memory` and `swap` (bytes), `pressure` (PSI avg60 percentages), `disks` |
| `processes`   | `processes`   | `list` of processes (`pid`, `ppid`, `name`, `exe`, redacted `cmdline`, `user`, `uid`, `state`, `cpuPercent`, `memoryBytes` (RSS), `threads`, `startedAt`) and a `kernelThreads` count. Hostprint's own process tree is excluded. |
| `network`     | `network`     | `interfaces` (`name`, `state`, `mac`, `mtu`, CIDR `addresses`, `virtual`), `listening` sockets (`protocol`, `address`, `port`, `pid`, `process`), `tcpStates` counts, `defaultGateways`, `dns` (`nameservers`, `search`, `upstreamNameservers`), `ephemeralPorts` |
| `services`    | `services`    | systemd service units, or launchd jobs on macOS (`serviceType` `launchd`, `result` such as `exit 78` or `signal 9`): `name`, `description`, `loadState`, `activeState`, `subState`, `serviceType`, `restarts` (`NRestarts`), `result`, `activeSince`, `mainPid` |
| `docker`      | `docker`      | `engineVersion` and `containers` (`id`, `name`, `image`, `imageId`, `state`, `status`, `health`, `restartCount`, `exitCode`, `oomKilled`, `startedAt`, `ports`, `memoryBytes`, `memoryLimitBytes`, `composeProject`, `composeService`) |
| `git`         | `git`         | `root`, `branch`, `commit`, `commitSubject`, `commitTime`, `describe`, credential-free `remote`, `dirty`, `staged`, `modified`, `untracked`, `changedPaths` (at most 100; never contents) |
| `runtimes`    | `runtimes`    | `name`, `version`, `path` for runtimes found on `PATH` |
| `environment` | `environment` | `fingerprintKeyId` and `variables` (`name`, `source`, `value`, `redacted`, `fingerprint`) |
| `files`       | `files`       | `path`, `exists`, `size`, `modified`, `sha256`, `mode`, `uid`, `gid`, `error` |
| `logs`        | `logs`        | `since` (start of the window) and `sources`: `kind` (`journal`, `docker`, `file`), `name`, `total`, `errors` and `warnings` (counted over the whole window), `topErrors` (`pattern`, `count`, `example`), `lines` (most recent, redacted, oldest first), `truncated` |

Byte quantities are plain integers in bytes. Disk `usedBytes` excludes
reserved blocks the same way `df` does.

Log collection is opt-in. Bounds: lines kept per source (`[logs] lines`,
default 50), 500 characters per line, 100 sources, 5000 journal entries,
the last 2000 lines per container, and the last 256 KiB of each log file.
`topErrors` patterns replace every word containing a digit with `#`, so
"timeout after 3012ms on 10.0.0.5" and "timeout after 87ms on 10.0.0.6" are
one pattern.

## Redacted values

```json
{ "name": "DATABASE_URL", "source": "process",
  "value": "postgres://app:[REDACTED]@db.internal:5432/app",
  "redacted": true, "fingerprint": "5c0f9e1d2a7b3c44" }
```

- `value` is `[REDACTED]`, or the original with only the credential removed
  (URL passwords, secret query parameters).
- `fingerprint` is the first 16 hex characters of HMAC-SHA256 of the original
  value, keyed with the installation's `fingerprint.key`. Equal fingerprints
  mean equal values; the fingerprint cannot be reversed or brute-forced
  without that key.
- `fingerprintKeyId` identifies the key, so the diff only compares
  fingerprints made with the same one.
- Values longer than 1 KiB that are not secret are stored as
  `"[<n> bytes]"` plus a fingerprint.
- `source` is `process` for Hostprint's own environment, or the path of the
  dotenv file the variable came from.
