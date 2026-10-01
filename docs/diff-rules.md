# Diff rules

`hostprint diff` is deterministic and rule-based. Every change it reports
names the rule that produced it (`rule` in `--json` output), so any
classification can be traced back to a line in this document and a function
in `crates/hostprint-diff`.

Significance levels:

| Level  | Meaning                                                          |
| ------ | ---------------------------------------------------------------- |
| HIGH   | Something that is commonly the direct cause or sign of an outage |
| MEDIUM | A meaningful change worth checking early                         |
| LOW    | A real change that is usually not the cause                      |
| INFO   | Expected churn; hidden unless `--all` or `--min info`            |

Hostprint reports evidence, not conclusions. A HIGH change means "look here
first", not "this caused the incident".

## Noise reduction

Some values change on every capture. They are never compared directly:

- PIDs, uptime, capture timestamps, per-process CPU percentages.
- Memory, CPU, load and disk figures, except across the thresholds below.
- Processes are grouped by name; a restart with a new PID is one LOW change,
  not "removed" plus "added".
- Processes younger than 60 seconds at capture time, and interactive tools
  (shells, `sshd`, `sudo`, editors, `top`, ...), are INFO.
- Listening ports inside the kernel's ephemeral range are INFO.
- Virtual interfaces (`veth*`, bridges, `lo`) are INFO; IPv6 link-local
  addresses are ignored.
- Terminal and session variables (`PWD`, `SHLVL`, `SSH_*`, `TERM_PROGRAM_*`,
  ...) are INFO.
- systemd `oneshot` units changing between inactive and active are INFO, and
  inactive units vanishing (systemd unloads them) are INFO.
- Container port bindings are compared only when the container is running in
  both snapshots.
- A section that could not be collected in one snapshot is not compared at
  all, so an unreachable Docker daemon does not look like every container
  being removed. See `collector.failed` below.
- `[ignore]` in `config.toml` removes processes, ports, environment
  variables, containers and services from the comparison (`*` wildcards
  allowed).

## Rules

### Collection

| Rule               | Level  | When                                                         |
| ------------------ | ------ | ------------------------------------------------------------ |
| `collector.failed` | MEDIUM | A collector that produced data before now fails (e.g. Docker daemon not reachable) |

A collector that is skipped (not installed, not a Git repository, turned off
with `--skip` or `[collectors] disable`) only adds a note.

### System

| Rule                  | Level        | When                                       |
| --------------------- | ------------ | ------------------------------------------ |
| `system.hostname`     | MEDIUM       | Hostname changed                           |
| `system.os`           | MEDIUM       | OS release changed                         |
| `system.kernel`       | MEDIUM / LOW | Major.minor changed / patch changed        |
| `system.architecture` | MEDIUM       | Machine architecture changed               |
| `system.reboot`       | MEDIUM       | Boot time moved by more than 60 s          |
| `system.timezone`     | LOW          | Timezone changed                           |
| `system.hardware`     | LOW          | DMI vendor or product changed              |
| `system.container`    | INFO         | Container runtime around Hostprint changed |

### Resources

| Rule               | Level  | When |
| ------------------ | ------ | ---- |
| `memory.total`     | MEDIUM | Total memory changed by more than 2% |
| `memory.available` | HIGH   | Fell by at least 20% and is now under 10% of total |
|                    | MEDIUM | Fell by at least 50% |
|                    | LOW    | Fell by at least 20% |
|                    | INFO   | Rose by at least 50% |
| `swap.total`       | LOW    | Swap size changed |
| `swap.used`        | MEDIUM / LOW | Grew by at least 256 MiB and at least doubled; MEDIUM if at least half of swap is now in use |
| `cpu.cores`        | MEDIUM | Logical core count changed |
| `cpu.usage`        | MEDIUM | Sampled busy time at least 90%, from under 70% |
|                    | LOW    | At least 70%, from under 40% |
| `cpu.iowait`       | LOW    | At least 20%, from under 5% |
| `cpu.steal`        | LOW    | At least 10%, from under 2% |
| `load.average`     | HIGH   | 1-minute load per core crossed 2.0 |
|                    | MEDIUM | Crossed 1.0 |
|                    | LOW    | Doubled and is at least 0.5 per core |
|                    | INFO   | Fell back below 1.0 |
| `pressure.stall`   | HIGH   | Memory "full" stall (PSI avg60) crossed 10% |
|                    | MEDIUM | Memory "some" crossed 10%, I/O "full" crossed 10%, or I/O "some" crossed 25% |
|                    | LOW    | CPU "some" crossed 50% |
| `disk.unresponsive`| HIGH   | Filesystem stopped answering `statvfs` within 2 s (hung NFS, failing disk) |
| `disk.responsive`  | LOW    | Filesystem answers again |
| `disk.read_only`   | HIGH / LOW | Remounted read-only / read-write |
| `disk.usage`       | HIGH   | Usage crossed 95% |
|                    | MEDIUM | Crossed 90% |
|                    | LOW    | Grew by at least 5 percentage points |
|                    | INFO   | Shrank by at least 10 points |
| `disk.inodes`      | HIGH / MEDIUM | Inode usage crossed 95% / 90% |
| `disk.device`      | MEDIUM | A mount point is backed by a different device or filesystem |
| `disk.unmounted`   | MEDIUM | Filesystem no longer mounted |
| `disk.mounted`     | LOW    | New filesystem mounted |
| `disk.size`        | LOW    | Filesystem size changed by more than 1% |

Disk usage is `used / (used + available)`, as `df` reports it.

### Processes

Processes are grouped by name.

| Rule                      | Level  | When |
| ------------------------- | ------ | ---- |
| `process.disappeared`     | MEDIUM | No process with this name any more (INFO if transient or interactive) |
| `process.appeared`        | LOW    | New process name (INFO if transient or interactive) |
| `process.count`           | LOW    | Instances halved (from at least 2), or doubled with at least 5 more; otherwise INFO |
| `process.memory`          | MEDIUM | Resident memory grew by at least 256 MiB and doubled |
|                           | LOW    | Grew by at least 128 MiB and 50% |
| `process.restarted`       | LOW    | Single-instance process has a new start time |
| `process.exe`             | LOW    | Single-instance process runs a different executable |
| `process.uninterruptible` | MEDIUM | At least 5 processes in D state, from fewer than 5 |
| `process.zombies`         | LOW    | At least 5 zombies, and more than before |
| `process.total`           | INFO   | Process count changed by at least 10 and 10% |

### Network

| Rule                         | Level  | When |
| ---------------------------- | ------ | ---- |
| `network.listener_removed`   | HIGH   | A TCP port is no longer listened on (LOW for UDP) |
| `network.listener_added`     | MEDIUM | New TCP listening port (LOW for UDP) |
| `network.listener_owner`     | MEDIUM | A different process owns the port (compared only when both snapshots know the owner) |
| `network.listener_exposed`   | MEDIUM | Port now bound to all interfaces (`0.0.0.0` / `::`) |
| `network.listener_address`   | LOW    | Other bind address change |
| `network.interface_down`     | HIGH   | Physical interface was up and no longer is |
| `network.interface_state`    | LOW    | Other operational state change |
| `network.interface_address`  | MEDIUM | Interface addresses changed |
| `network.interface_removed`  | MEDIUM | Interface disappeared |
| `network.interface_added`    | LOW    | New interface |
| `network.interface_mtu`      | LOW    | MTU changed |
| `network.interface_mac`      | LOW    | MAC address changed |
| `network.gateway`            | MEDIUM | Default routes changed |
| `network.dns`                | MEDIUM | DNS servers (or upstream servers behind a local stub resolver) changed |
| `network.dns_search`         | LOW    | Search domains changed |
| `network.tcp_state`          | MEDIUM | CLOSE_WAIT ≥ 50, SYN_SENT ≥ 50 or SYN_RECV ≥ 100, at least tripled |
|                              | LOW    | TIME_WAIT ≥ 1000 or ESTABLISHED ≥ 100, at least tripled; or ESTABLISHED fell to a third (from at least 20) |

Interface rules are INFO for virtual interfaces, and listener rules are INFO
for ports in the ephemeral range.

### Services (systemd)

| Rule                   | Level  | When |
| ---------------------- | ------ | ---- |
| `service.failed`       | HIGH   | Unit entered the failed state (or appeared already failed) |
| `service.restart_loop` | HIGH   | Unit is waiting to be restarted (`activating (auto-restart)`) |
| `service.stopped`      | HIGH   | Unit went from active to inactive (INFO for oneshot) |
| `service.restarts`     | HIGH   | systemd restarted it 5 or more times since the baseline |
|                        | MEDIUM | 1 to 4 times |
| `service.restarted`    | LOW    | Still running but started again without an automatic restart |
| `service.recovered`    | LOW    | Failed → active |
| `service.started`      | LOW    | Became active (INFO for oneshot) |
| `service.state`        | LOW    | Any other state change (INFO for oneshot) |
| `service.removed`      | MEDIUM | An active unit is no longer loaded (INFO if it was inactive) |
| `service.added`        | LOW    | New active unit (INFO if inactive) |

### Containers (Docker)

Containers are matched by name.

| Rule                       | Level  | When |
| -------------------------- | ------ | ---- |
| `container.state`          | HIGH   | Left the running state |
|                            | LOW    | Became running |
|                            | MEDIUM | Other state change |
| `container.health`         | HIGH   | Became unhealthy |
|                            | MEDIUM | Healthy → starting |
|                            | LOW    | Other health change |
| `container.restarts`       | HIGH   | Restart count grew by 3 or more, or the container is restarting |
|                            | MEDIUM | Grew by 1 or 2 |
| `container.oom_killed`     | HIGH   | Container was OOM-killed |
| `container.memory_limit`   | HIGH   | Memory use crossed 90% of its limit |
| `container.memory`         | MEDIUM / LOW | Same thresholds as `process.memory` |
| `container.removed`        | HIGH   | A running container no longer exists (LOW if it was stopped) |
| `container.added`          | LOW    | New container (MEDIUM if restarting or unhealthy) |
| `container.exit_code`      | MEDIUM | New non-zero exit code; 137, 139, 143 and others are annotated |
| `container.image`          | MEDIUM | Different image reference |
| `container.image_id`       | MEDIUM | Same tag, different image (rebuilt or re-pulled) |
| `container.ports`          | MEDIUM | Port bindings changed (both running) |
| `container.recreated`      | LOW    | Same name, new container ID |
| `container.restarted`      | LOW    | Same container started again without a restart-count change |
| `docker.engine`            | LOW    | Docker Engine version changed |

### Application

| Rule              | Level        | When |
| ----------------- | ------------ | ---- |
| `git.commit`      | MEDIUM       | HEAD commit changed |
| `git.branch`      | MEDIUM       | Branch changed (or HEAD detached) |
| `git.dirty`       | MEDIUM       | Clean → uncommitted changes to tracked files |
| `git.clean`       | LOW          | Uncommitted changes → clean |
| `git.changes`     | LOW          | The set of changed tracked files differs |
| `git.remote`      | LOW          | `origin` URL changed |
| `git.untracked`   | INFO         | Untracked file count changed |
| `git.root`        | INFO         | Snapshots describe different repositories |
| `runtime.version` | MEDIUM / LOW | Major version / other version change |
| `runtime.removed` | MEDIUM       | Runtime no longer on `PATH` |
| `runtime.added`   | LOW          | New runtime on `PATH` |
| `runtime.path`    | LOW          | A different binary is first on `PATH` |

### Configuration

| Rule                 | Level  | When |
| -------------------- | ------ | ---- |
| `env.changed`        | MEDIUM | Value changed |
| `env.secret_changed` | MEDIUM | A redacted value's fingerprint changed |
| `env.removed`        | MEDIUM | Variable no longer set |
| `env.added`          | LOW    | New variable |
| `env.path`           | LOW    | `PATH` changed (entries added and removed are listed) |
| `env.redaction`      | INFO   | Stored redacted in one snapshot and not the other |

Session and terminal variables are INFO. Fingerprints are compared only when
both snapshots used the same fingerprint key (the same Hostprint home);
otherwise a note says how many could not be compared.

### Files

| Rule             | Level  | When |
| ---------------- | ------ | ---- |
| `file.removed`   | HIGH   | A tracked file no longer exists |
| `file.content`   | MEDIUM | SHA-256 changed |
| `file.created`   | LOW    | A tracked file now exists |
| `file.mode`      | LOW    | Permission bits changed |
| `file.owner`     | LOW    | Owner or group changed |
| `file.error`     | LOW    | The file could not be read in one snapshot |
| `file.touched`   | INFO   | Modification time changed, content did not |
| `file.tracked` / `file.untracked` | INFO | The file is only configured in one snapshot |

### Logs

Compared only when both snapshots collected logs (`--logs-since` or
`collect_logs = true`). Sources are matched by kind and name: a journal unit,
a container, or a file. A source with no lines in the window counts as zero
errors.

| Rule            | Level  | When |
| --------------- | ------ | ---- |
| `log.errors`    | MEDIUM | Error lines appeared where there were none, or at least tripled and grew by 10 or more |
|                 | LOW    | Error lines increased |
|                 | INFO   | Error lines went back to zero |
| `log.new_error` | LOW    | An error message (with numbers and IDs normalised away) that wasn't among the source's top errors before; up to 3 per source |

Journal lines at priority `err` or worse count as errors and `warning` as
warnings; only those priorities are collected. For container output and log
files, a line is an error if it contains a word such as `error`, `fatal`,
`panic`, `exception` or `traceback`. If the two snapshots used log windows of
different lengths, a note says the counts are not directly comparable.
