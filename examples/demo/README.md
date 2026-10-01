# Hostprint demo

A reproducible incident: a healthy API + Postgres + Redis stack is captured,
then broken in four ways, captured again, and compared.

| What breaks                    | How                                          |
| ------------------------------ | -------------------------------------------- |
| Redis enters a restart loop    | `broken.yml` gives it a command that exits 1 |
| Memory pressure                | a new `hog` container holds ~1.2 GiB         |
| Configuration drift            | `DATABASE_POOL_SIZE` 10 → 50, a rotated secret |
| A new application commit       | a commit lands in the demo repository        |

## Run it

On a Linux machine with Docker (Compose v2) and git:

```sh
cargo install --path ../../crates/hostprint-cli   # or put a release binary on PATH
./run-demo.sh
```

The script uses a temporary `HOSTPRINT_HOME`, so it never touches your own
snapshots, and removes the stack when it finishes (`KEEP=1` keeps it).

Container changes come from the Docker API. Run on the Docker host itself,
Hostprint also sees the processes inside the containers (`redis-server`
disappearing, the hog's resident memory); run inside a container, it sees only
that container's processes.
