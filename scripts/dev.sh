#!/usr/bin/env bash
# Build, test and run Hostprint inside Docker, without a local Rust toolchain.
#
#   ./scripts/dev.sh check            fmt check, clippy and tests: what CI runs
#   ./scripts/dev.sh test [args]      cargo test --workspace [args]
#   ./scripts/dev.sh fmt              cargo fmt --all
#   ./scripts/dev.sh build            static release binary in dist/hostprint
#   ./scripts/dev.sh run [args]       run the CLI, e.g. run capture --name healthy
#   ./scripts/dev.sh demo             build, then run examples/demo against your Docker
#   ./scripts/dev.sh shell            interactive shell in the toolchain container
#   ./scripts/dev.sh cargo [args]     any other cargo command
#   ./scripts/dev.sh clean            remove the cache volumes and the dev image
#
# The toolchain image (hostprint-dev) is built on first use. Cargo's registry,
# build output and the snapshots made with "run" persist in the Docker volumes
# hostprint-cargo, hostprint-target and hostprint-home.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
image=hostprint-dev
volumes=(-v "$root:/src" -v hostprint-cargo:/usr/local/cargo/registry -v hostprint-target:/target)
docker_socket=(-v /var/run/docker.sock:/var/run/docker.sock)
# shellcheck disable=SC2016 # expanded inside the container, not here
static_build='set -e; t="$(uname -m)-unknown-linux-musl"; cargo build --release --locked --target "$t"; mkdir -p /src/dist; cp "/target/$t/release/hostprint" /src/dist/hostprint; ls -l /src/dist/hostprint'

usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; }

command=${1:-help}
shift || true

if [[ $command == help || $command == -h || $command == --help ]]; then
    usage
    exit 0
fi

docker info >/dev/null 2>&1 || { echo "Docker is not running." >&2; exit 1; }

if [[ $command == clean ]]; then
    docker volume rm hostprint-cargo hostprint-target hostprint-home >/dev/null 2>&1 || true
    docker image rm "$image" >/dev/null 2>&1 || true
    echo "Removed the hostprint-dev image and cache volumes."
    exit 0
fi

if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "Building the $image toolchain image (first run only)..."
    docker build -t "$image" -f "$root/docker/dev.Dockerfile" "$root/docker"
fi

tty=()
[[ -t 1 ]] && tty=(-t)

dev() { docker run --rm "${volumes[@]}" "${tty[@]}" "$@"; }

case $command in
    check) dev "$image" sh -c 'cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace' ;;
    test)  dev "$image" cargo test --workspace "$@" ;;
    fmt)   dev "$image" cargo fmt --all ;;
    build) dev "$image" sh -c "$static_build" ;;
    run)   dev "${docker_socket[@]}" -v hostprint-home:/root/.hostprint "$image" cargo run -q -p hostprint -- "$@" ;;
    demo)
        dev "$image" sh -c "$static_build"
        docker run --rm -v "$root:/src" "${docker_socket[@]}" -e HOSTPRINT=/src/dist/hostprint docker:cli \
            sh -c 'apk add -q git >/dev/null && sh /src/examples/demo/run-demo.sh'
        ;;
    shell) docker run --rm -it "${volumes[@]}" "${docker_socket[@]}" -v hostprint-home:/root/.hostprint "$image" bash ;;
    cargo) dev "$image" cargo "$@" ;;
    *) echo "Unknown command '$command'. Run ./scripts/dev.sh help" >&2; exit 2 ;;
esac
