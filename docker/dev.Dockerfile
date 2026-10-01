# Toolchain image for developing Hostprint without a local Rust install.
# Built automatically by scripts/dev.ps1 and scripts/dev.sh.
FROM rust:1

RUN rustup component add clippy rustfmt \
 && rustup target add "$(uname -m)-unknown-linux-musl" aarch64-apple-darwin x86_64-apple-darwin \
 && apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*

# Build output lives in a named volume, not in the bind-mounted source tree:
# much faster on Docker Desktop, and keeps Linux artifacts off the host.
ENV CARGO_TARGET_DIR=/target
WORKDIR /src
