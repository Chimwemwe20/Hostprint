#!/bin/sh
# Reproducible Hostprint demo: capture a healthy stack, break it, capture
# again, and diff.
#
# Requires Linux, Docker with Compose v2, git, and `hostprint` on PATH
# (or HOSTPRINT=/path/to/hostprint). Everything is removed afterwards unless
# KEEP=1 is set; OUT=dir keeps just the incident bundle. Snapshots go to a
# temporary HOSTPRINT_HOME, not ~/.hostprint.
set -eu

cd "$(dirname "$0")"
HOSTPRINT=${HOSTPRINT:-hostprint}
WORK=$(mktemp -d)
export HOSTPRINT_HOME="$WORK/hostprint-home"
COMPOSE="docker compose -f docker-compose.yml -f broken.yml"

cleanup() {
    if [ "${KEEP:-0}" = 1 ]; then
        echo "Kept the stack and $WORK (remove with: $COMPOSE down -v)"
    else
        $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

git_app() {
    git -C "$WORK/app" -c user.name=demo -c user.email=demo@example.com "$@"
}

capture() {
    "$HOSTPRINT" capture --name "$1" --force --quiet --logs-since 10m \
        --repo "$WORK/app" --env-file "$WORK/app/.env" >/dev/null
    echo "    captured '$1'"
}

echo "==> Starting the healthy stack"
docker compose -f docker-compose.yml up -d --wait --quiet-pull

echo "==> Creating the demo application repository"
mkdir -p "$WORK/app"
cp app.env "$WORK/app/.env"
git_app init -q
git_app commit -q --allow-empty -m "Release 1.4.0"

echo "==> Capturing the healthy state"
capture healthy

echo "==> Breaking things"
$COMPOSE up -d --quiet-pull
sed -i 's/^DATABASE_POOL_SIZE=.*/DATABASE_POOL_SIZE=50/; s/^JWT_SECRET=.*/JWT_SECRET=rotated-signing-secret/' "$WORK/app/.env"
git_app commit -q --allow-empty -m "Raise DATABASE_POOL_SIZE to 50"
echo "    waiting 25s for redis to crash-loop..."
sleep 25

echo "==> Capturing the broken state"
capture broken

echo
"$HOSTPRINT" diff healthy broken

echo
echo "==> Bundling the evidence"
"$HOSTPRINT" bundle broken --against healthy --output "$WORK/incident.tar.gz" 2>/dev/null
if [ -n "${OUT:-}" ]; then
    cp "$WORK/incident.tar.gz" "$OUT/"
    echo "    copied to $OUT/incident.tar.gz"
fi
