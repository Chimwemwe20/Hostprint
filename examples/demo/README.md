# Hostprint demo

A reproducible incident: a healthy API + Postgres + Redis stack is captured,
then broken in four ways, captured again, compared, and bundled.

| What breaks                    | How                                            |
| ------------------------------ | ---------------------------------------------- |
| Redis enters a restart loop    | `broken.yml` gives it a command that exits 1   |
| Memory pressure                | a new `hog` container holds ~1.2 GiB           |
| Configuration drift            | `DATABASE_POOL_SIZE` 10 → 50, a rotated secret |
| A new application commit       | a commit lands in the demo repository          |

Both captures include the last 10 minutes of container logs, so the diff
also shows Redis's error lines and the new error message itself. The script
ends by writing an incident bundle (`hostprint bundle broken --against
healthy`).

## Run it

From the repository root, with only Docker installed:

```sh
./scripts/dev.sh demo        # or .\scripts\dev.ps1 demo on Windows
```

This builds a static `dist/hostprint` and runs `run-demo.sh` in a `docker:cli`
container against your Docker daemon.

Or directly, on a Linux machine with Docker (Compose v2), git, and `hostprint`
on `PATH` (or `HOSTPRINT=/path/to/hostprint`):

```sh
./run-demo.sh
```

The script uses a temporary `HOSTPRINT_HOME`, so it never touches your own
snapshots, and removes the stack and its files when it finishes. Set `KEEP=1`
to keep them, including the bundle.

Container changes and logs come from the Docker API. Run on the Docker host
itself, Hostprint also sees the processes inside the containers
(`redis-server` disappearing, the hog's resident memory). Run inside a
container, as `dev demo` does, it sees only that container's processes.
