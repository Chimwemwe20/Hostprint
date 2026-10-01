# Builds a static hostprint binary for the current architecture, without a
# local Rust toolchain:
#
#   docker build --target binary --output dist .
#
# The binary lands in dist/hostprint and runs on any Linux of the same
# architecture (x86_64 or aarch64).

FROM rust:1 AS build
RUN rustup target add "$(uname -m)-unknown-linux-musl" \
 && apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    target="$(uname -m)-unknown-linux-musl" \
 && cargo build --release --locked --target "$target" \
 && cp "target/$target/release/hostprint" /hostprint

FROM scratch AS binary
COPY --from=build /hostprint /hostprint
