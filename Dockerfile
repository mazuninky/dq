# Multi-stage alpine build for `dq`.
#
# Stage 1 builds the release binary against musl (alpine's libc). Stage 2
# ships only the binary on top of a minimal alpine runtime — no apk index,
# no build deps, non-root `dq` user.
#
# Build:   docker build -t dq:latest .
# Run:     docker run --rm -v $(pwd):/work dq:latest get config.yaml /name

# -----------------------------------------------------------------------------
# Stage 1: build
# -----------------------------------------------------------------------------
FROM rust:1.94-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY clippy.toml deny.toml ./
COPY crates ./crates

RUN cargo build --release --locked --bin dq

# -----------------------------------------------------------------------------
# Stage 2: runtime
# -----------------------------------------------------------------------------
FROM alpine:3.21

RUN addgroup -S -g 1000 dq && adduser -S -u 1000 -G dq dq

COPY --from=builder /src/target/release/dq /usr/local/bin/dq

WORKDIR /work
USER dq

LABEL org.opencontainers.image.source="https://github.com/mazuninky/dq" \
      org.opencontainers.image.description="Agent-friendly Rust CLI for structured data + linter platform" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["/usr/local/bin/dq"]
CMD ["--help"]
