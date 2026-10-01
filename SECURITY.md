# Security

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository, not in a public issue. Include the Hostprint version,
your platform, and steps to reproduce. We aim to acknowledge reports within a
few days.

Secret redaction misses (a credential that ends up stored in a snapshot) are
security bugs and are welcome as reports.

## What Hostprint does and does not do

**It stays local.** Hostprint makes no network connections of its own. The
only socket it opens is the local Docker Engine socket, when one exists. There
is no telemetry, no update check, and no upload; snapshots leave the machine
only if you copy them. The one exception is the one you ask for: capturing
`ssh://host` runs your system `ssh` client to that host.

**Remote capture leaves nothing behind.** For `ssh://` targets Hostprint
streams a tar archive on stdin (its own binary and a fingerprint key) into a
`mktemp -d` directory created under `umask 077`, runs the capture, and removes
the directory from a shell `trap`, on success or failure. If the connection
drops mid-capture, that directory (under `/tmp`, or `$TMPDIR`) may remain. The
key sent is derived from your local key and the host name, so captures of one
host can compare secret fingerprints while no remote ever holds your local
key. Nothing secret is put on a command line, where other users of the remote
could read it with `ps`. Destinations are validated, and options are ended
with `--` before the destination, so a crafted `ssh://-o…` cannot inject ssh
options.

**It reads metadata, not content, by default.** File fingerprints are hashes
and Git data is commit and file *names*. Logs are the one kind of content
Hostprint can collect, and only when asked (`--logs-since`, or
`collect_logs = true`). They are bounded by time window, lines per source and
line length, and every line is redacted.

**It redacts before storing.** Redaction runs inside each collector, so secret
values never reach a snapshot in memory or on disk. A value is redacted when:

- its name contains `PASSWORD`, `PASSWD`, `PASS`, `SECRET`, `TOKEN`, `KEY`,
  `AUTH`, `COOKIE`, `PRIVATE`, `CREDENTIAL`, `SESSION`, `SIGNATURE`, `SALT` or
  `DSN` (case-insensitive, plus anything in `[redact] patterns`);
- it is a URL with a password, or has secret-named query parameters (only the
  secret part is removed);
- it looks like a credential: a known token prefix (`ghp_`, `sk_live_`,
  `xoxb-`, `AKIA`, `glpat-`, ...), a JWT, a PEM block, or a long string mixing
  upper case, lower case and digits.

This applies to environment variables, dotenv files and the Git remote URL.
Process command lines and log lines are treated as free text: secret-named
`key=value` and `"key": "value"` pairs, a secret key followed by its value
(`password: hunter2`, `Authorization: Bearer …`), credential URLs and
token-shaped words are redacted wherever they appear, including inside an
inline script such as `sh -c '…'`.

**Fingerprints are keyed.** Redacted values keep a truncated
HMAC-SHA256 fingerprint so a diff can report "this secret changed". The key
is 32 random bytes in `~/.hostprint/fingerprint.key`, created on first use.
Without it, a fingerprint cannot be checked against guesses, so a shared
snapshot does not expose low-entropy secrets to brute force.

**Storage is private.** `~/.hostprint` is created `0700`; snapshots,
baselines and the key are written `0600`, atomically. Exports and bundles,
which are written outside it, are also `0600`, and Hostprint reminds you to
review them before sharing.

**It does not need root.** Without root, details of other users' processes and
socket owners are left out and the capture says so. Hostprint never tries to
elevate itself.

## Limits to be aware of

- Redaction is heuristic. A secret stored under an innocuous name with an
  innocuous shape (`MY_SETTING=hunter2`) is not detected. Add such names (or
  a fragment of them) to `[redact] patterns` so they are stored only as
  fingerprints.
- Snapshots still describe your infrastructure: hostnames, IP addresses,
  process names and command lines, container names, file paths. Treat them as
  internal documents and review them before sharing outside your team.
- Log lines are free-form. Personal data in them (email addresses, customer
  names, IP addresses of users) is not redacted. Collect logs only when you
  need them, and review a bundle's `logs/` before sending it anywhere.
- The runtime collector runs `--version` (or equivalent) for tools found on
  `PATH`, such as `node`, `python3` and `java`, with a timeout. Run Hostprint
  with a `PATH` you trust.
- Reading the Docker socket requires membership of the `docker` group or
  root, which is root-equivalent on most systems. Hostprint only issues
  read-only `GET` requests.
- Setting `redact_secrets = false` in `config.toml` stores values verbatim.
