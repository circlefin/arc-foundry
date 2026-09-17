# syntax=docker/dockerfile:1

FROM rust:1-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 AS chef
WORKDIR /app

# Use bash with pipefail so a failure in a piped RUN step is not masked.
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential libssl-dev git pkg-config curl perl libclang-dev \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    BINSTALL_VERSION="v1.18.1"; \
    case "$(dpkg --print-architecture)" in \
      amd64) ARCH="x86_64-unknown-linux-musl"; SHA256="cf2a4b54494ea8555d6349685e9a301efc1051d9fba6308c76914b2486f8700f" ;; \
      arm64) ARCH="aarch64-unknown-linux-musl"; SHA256="c55962a0115f9716b709216de7f8bdd59d6ba8738779e60b051b4593f677717a" ;; \
      *) echo "unsupported architecture" >&2; exit 1 ;; \
    esac; \
    curl -L --proto '=https' --tlsv1.2 -sSf \
      "https://github.com/cargo-bins/cargo-binstall/releases/download/${BINSTALL_VERSION}/cargo-binstall-${ARCH}.tgz" \
      -o /tmp/cargo-binstall.tgz; \
    echo "${SHA256}  /tmp/cargo-binstall.tgz" | sha256sum -c -; \
    tar -xzf /tmp/cargo-binstall.tgz -C /usr/local/cargo/bin cargo-binstall; \
    rm /tmp/cargo-binstall.tgz
RUN cargo binstall -y cargo-chef

# Prepare the cargo-chef recipe.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Build the project.
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# `cargo chef cook` builds only dependencies from recipe.json, but it still reads
# the root manifest, which needs two things cargo-chef does not reconstruct:
#   - vendor/: the `ctutils` [patch] points at the local vendor/ctutils-compat crate.
#   - rust-toolchain.toml: pins rustc 1.95.0; without it cook uses the base image's
#     older default toolchain and fails the crates' MSRV check.
COPY vendor/ vendor/
COPY rust-toolchain.toml rust-toolchain.toml

ARG RUST_PROFILE
ARG RUST_FEATURES

# sccache is intentionally NOT used here. It errors under the cargo-chef +
# proc-macro layout ("Failed to open file for hashing libfoundry_macros-*.so")
# on the dist profile. BuildKit cache mounts + the cached cook layer already
# provide cross-build caching, so dropping sccache costs little.
ENV CARGO_INCREMENTAL=0 \
    CARGO_NET_GIT_FETCH_WITH_CLI=true

# Build dependencies.
#
# The `github_token` secret is optional. When present it lets cargo authenticate
# the clone of a private git dependency. When absent (e.g. builds whose
# dependency resolves to a public repo) the block is skipped and the clone
# proceeds unauthenticated.
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=shared \
    set -eu; \
    if [ -s /run/secrets/github_token ]; then \
        token="$(cat /run/secrets/github_token)"; \
        git config --global url."https://${token}@github.com/".insteadOf "https://github.com/"; \
        trap 'git config --global --remove-section url."https://${token}@github.com/" || true' EXIT; \
    fi; \
    cargo chef cook --recipe-path recipe.json --profile ${RUST_PROFILE} --no-default-features --features "${RUST_FEATURES}"

ARG TAG_NAME="dev"
ENV TAG_NAME=$TAG_NAME
ARG VERGEN_GIT_SHA="ffffffffffffffffffffffffffffffffffffffff"
ENV VERGEN_GIT_SHA=$VERGEN_GIT_SHA

# Build the project.
COPY . .
RUN --mount=type=secret,id=github_token,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=shared \
    set -eu; \
    if [ -s /run/secrets/github_token ]; then \
        token="$(cat /run/secrets/github_token)"; \
        git config --global url."https://${token}@github.com/".insteadOf "https://github.com/"; \
        trap 'git config --global --remove-section url."https://${token}@github.com/" || true' EXIT; \
    fi; \
    cargo build --profile ${RUST_PROFILE} --no-default-features --features "${RUST_FEATURES}"

# `dev` profile outputs to the `target/debug` directory.
RUN ln -s /app/target/debug /app/target/dev \
    && mkdir -p /app/output \
    && mv \
    /app/target/${RUST_PROFILE}/forge \
    /app/target/${RUST_PROFILE}/cast \
    /app/target/${RUST_PROFILE}/anvil \
    /app/target/${RUST_PROFILE}/chisel \
    /app/output/

FROM ubuntu:22.04@sha256:eb29ed27b0821dca09c2e28b39135e185fc1302036427d5f4d70a41ce8fd7659 AS runtime

# Install runtime dependencies. ca-certificates is required for cast/forge to
# make any RPC/HTTPS call (their HTTP client loads the system trust store at
# startup and errors on an empty one). It is not in the ubuntu:22.04 base and
# was previously pulled in only as a Recommends of git, which
# --no-install-recommends now excludes, so install it explicitly.
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/output/* /usr/local/bin/

RUN groupadd -g 1000 foundry && \
    useradd -m -u 1000 -g foundry foundry
USER foundry

ENTRYPOINT ["/bin/sh", "-c"]

LABEL org.label-schema.build-date=$BUILD_DATE \
      org.label-schema.name="Foundry" \
      org.label-schema.description="Foundry" \
      org.label-schema.url="https://getfoundry.sh" \
      org.label-schema.vcs-ref=$VCS_REF \
      org.label-schema.vcs-url="https://github.com/foundry-rs/foundry.git" \
      org.label-schema.vendor="Foundry-rs" \
      org.label-schema.version=$VERSION \
      org.label-schema.schema-version="1.0"
